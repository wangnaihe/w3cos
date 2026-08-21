//! Shared heap accounting for AOT, W3VM and Host-created Core values.
//!
//! This is deliberately part of `w3cos-core`: execution backends enter an
//! owner scope, while ordinary `Value` allocation sites report through the
//! same tickets. The counters are thread-local because Core heap values use
//! `Rc` and are therefore confined to their JavaScript runtime thread.

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapKind {
    Object,
    Array,
    Function,
}

impl HeapKind {
    const fn index(self) -> usize {
        match self {
            Self::Object => 0,
            Self::Array => 1,
            Self::Function => 2,
        }
    }
}

/// A point-in-time view of estimated Core heap residency.
///
/// Byte values include Core containers and their owned property/element
/// storage. Opaque memory captured by Rust Host closures and external native
/// resources is intentionally outside this estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeapSnapshot {
    pub live_bytes: usize,
    pub peak_live_bytes: usize,
    pub live_objects: usize,
    pub live_arrays: usize,
    pub live_functions: usize,
    pub total_allocations: u64,
    pub total_allocated_bytes: u64,
}

impl HeapSnapshot {
    pub fn live_allocations(self) -> usize {
        self.live_objects
            .saturating_add(self.live_arrays)
            .saturating_add(self.live_functions)
    }
}

#[derive(Default)]
struct HeapCounters {
    live_bytes: usize,
    peak_live_bytes: usize,
    live_by_kind: [usize; 3],
    total_allocations: u64,
    total_allocated_bytes: u64,
}

impl HeapCounters {
    fn snapshot(&self) -> HeapSnapshot {
        HeapSnapshot {
            live_bytes: self.live_bytes,
            peak_live_bytes: self.peak_live_bytes,
            live_objects: self.live_by_kind[HeapKind::Object.index()],
            live_arrays: self.live_by_kind[HeapKind::Array.index()],
            live_functions: self.live_by_kind[HeapKind::Function.index()],
            total_allocations: self.total_allocations,
            total_allocated_bytes: self.total_allocated_bytes,
        }
    }

    fn allocate(&mut self, kind: HeapKind, bytes: usize) {
        self.live_by_kind[kind.index()] = self.live_by_kind[kind.index()].saturating_add(1);
        self.live_bytes = self.live_bytes.saturating_add(bytes);
        self.peak_live_bytes = self.peak_live_bytes.max(self.live_bytes);
        self.total_allocations = self.total_allocations.saturating_add(1);
        self.total_allocated_bytes = self
            .total_allocated_bytes
            .saturating_add(bytes.try_into().unwrap_or(u64::MAX));
    }

    fn resize(&mut self, old_bytes: usize, new_bytes: usize) {
        if new_bytes >= old_bytes {
            let growth = new_bytes - old_bytes;
            self.live_bytes = self.live_bytes.saturating_add(growth);
            self.peak_live_bytes = self.peak_live_bytes.max(self.live_bytes);
            self.total_allocated_bytes = self
                .total_allocated_bytes
                .saturating_add(growth.try_into().unwrap_or(u64::MAX));
        } else {
            self.live_bytes = self.live_bytes.saturating_sub(old_bytes - new_bytes);
        }
    }

    fn deallocate(&mut self, kind: HeapKind, bytes: usize) {
        self.live_by_kind[kind.index()] = self.live_by_kind[kind.index()].saturating_sub(1);
        self.live_bytes = self.live_bytes.saturating_sub(bytes);
    }
}

thread_local! {
    static GLOBAL_HEAP: RefCell<HeapCounters> = RefCell::new(HeapCounters::default());
    static CURRENT_OWNER: RefCell<Option<Rc<RefCell<HeapCounters>>>> =
        const { RefCell::new(None) };
}

/// Returns process-thread-wide Core heap counters.
pub fn heap_snapshot() -> HeapSnapshot {
    GLOBAL_HEAP.with(|heap| heap.borrow().snapshot())
}

/// An accounting domain shared by every execution backend participating in
/// one page/Realm. Enter it around AOT, W3VM or Host calls that may allocate.
#[derive(Clone, Default)]
pub struct HeapOwner(Rc<RefCell<HeapCounters>>);

impl HeapOwner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enter(&self) -> HeapScope {
        let previous = CURRENT_OWNER.with(|current| current.replace(Some(Rc::clone(&self.0))));
        HeapScope { previous }
    }

    pub fn snapshot(&self) -> HeapSnapshot {
        self.0.borrow().snapshot()
    }
}

/// Restores the prior accounting owner when a nested execution segment ends.
#[must_use = "the heap scope must live for the full execution segment"]
pub struct HeapScope {
    previous: Option<Rc<RefCell<HeapCounters>>>,
}

impl Drop for HeapScope {
    fn drop(&mut self) {
        CURRENT_OWNER.with(|current| {
            *current.borrow_mut() = self.previous.take();
        });
    }
}

/// Lifetime ticket embedded in a Core heap allocation.
pub(crate) struct HeapAllocation {
    kind: HeapKind,
    bytes: std::cell::Cell<usize>,
    owner: Option<Rc<RefCell<HeapCounters>>>,
}

impl HeapAllocation {
    pub(crate) fn new(kind: HeapKind, bytes: usize) -> Self {
        let owner = CURRENT_OWNER.with(|current| current.borrow().clone());
        GLOBAL_HEAP.with(|heap| heap.borrow_mut().allocate(kind, bytes));
        if let Some(owner) = &owner {
            owner.borrow_mut().allocate(kind, bytes);
        }
        Self {
            kind,
            bytes: std::cell::Cell::new(bytes),
            owner,
        }
    }

    pub(crate) fn set_bytes(&self, new_bytes: usize) {
        let old_bytes = self.bytes.replace(new_bytes);
        if old_bytes == new_bytes {
            return;
        }
        GLOBAL_HEAP.with(|heap| heap.borrow_mut().resize(old_bytes, new_bytes));
        if let Some(owner) = &self.owner {
            owner.borrow_mut().resize(old_bytes, new_bytes);
        }
    }
}

impl Drop for HeapAllocation {
    fn drop(&mut self) {
        let bytes = self.bytes.get();
        let _ = GLOBAL_HEAP.try_with(|heap| heap.borrow_mut().deallocate(self.kind, bytes));
        if let Some(owner) = &self.owner {
            owner.borrow_mut().deallocate(self.kind, bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use std::collections::HashMap;

    #[test]
    fn owner_and_global_snapshots_track_shared_core_allocations() {
        let global_before = heap_snapshot();
        crate::page_arena::reset();
        let owner = HeapOwner::new();
        let owner_before = owner.snapshot();

        let (object, array, function) = {
            let _scope = owner.enter();
            (
                Value::object(HashMap::new()),
                Value::array(vec![Value::Number(1.0)]),
                Value::function(|_, _| Value::Undefined),
            )
        };

        let owned = owner.snapshot();
        assert!(owned.live_bytes > owner_before.live_bytes);
        assert_eq!(owned.live_objects - owner_before.live_objects, 2);
        assert_eq!(owned.live_arrays - owner_before.live_arrays, 1);
        assert_eq!(owned.live_functions - owner_before.live_functions, 1);
        assert_eq!(
            owned.live_allocations() - owner_before.live_allocations(),
            4
        );

        for index in 0..64 {
            object.set_property(
                &format!("object-property-{index}"),
                Value::Number(index as f64),
            );
            function.set_property(
                &format!("function-property-{index}"),
                Value::Number(index as f64),
            );
            array.call_method("push", vec![Value::Number(index as f64)]);
        }
        assert!(
            owner.snapshot().live_bytes > owned.live_bytes,
            "container growth must update the original allocation owner's residency"
        );

        let global = heap_snapshot();
        assert!(global.live_bytes > global_before.live_bytes);
        assert_eq!(
            global.live_allocations() - global_before.live_allocations(),
            4
        );

        drop((object, array, function));
        crate::page_arena::reset();
        assert_eq!(owner.snapshot().live_bytes, owner_before.live_bytes);
        assert_eq!(
            owner.snapshot().live_allocations(),
            owner_before.live_allocations()
        );
    }
}
