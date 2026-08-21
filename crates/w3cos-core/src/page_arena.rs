//! Page-scoped bump arena, interned string handles, and JS object/array/function slots.
//!
//! Short interned JS strings, page-local [`crate::JsObject`]s, constructor
//! / literal arrays, and ordinary JS closures created via [`crate::Value::function`]
//! live here so clones copy a `u32` handle instead of `Rc::clone`. The bump
//! and tables are dropped on [`reset`] (`reset_bridge` / document navigation).
//! Host / DOM wrappers keep the `Value::Object(Rc)` path so WeakRef intern
//! can drop unreferenced wrappers without a page reset. `array_hole` and host
//! arrays keep `Value::Array(Rc)` when they must be collectable without a
//! page reset. Host/DOM callables (`Value::callable`, jsdom call slots) stay
//! on the `Value::Function(Rc)` / object call-slot path. Size-class slabs
//! wait for a later cut.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::rc::Rc;

const CHUNK: usize = 4096;

/// Interned page-local string. `Clone` is a handle copy, not `Rc::clone`.
#[derive(Clone, Copy)]
pub(crate) struct PageString {
    pub handle: u32,
    pub epoch: u32,
    ptr: *const u8,
    len: u32,
    /// Thread-confine the raw bump pointer (raw pointers are `Send`/`Sync`).
    _thread: PhantomData<Rc<()>>,
}

impl PageString {
    pub(crate) fn as_str(&self) -> &str {
        if self.epoch != current_epoch() {
            panic!("page-interned string used after reset_bridge");
        }
        // Safety: `ptr`/`len` name UTF-8 bytes in a bump chunk that lives
        // until `reset` bumps the epoch. Callers must not hold the `&str` across
        // a page reset.
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len as usize))
        }
    }
}

struct PageArena {
    epoch: u32,
    chunks: Vec<Vec<u8>>,
    current: Vec<u8>,
    /// 1-based intern slots; index 0 is unused so handle `0` is never valid.
    slots: Vec<PageString>,
    intern: HashMap<u64, Vec<u32>>,
    allocated: usize,
    /// 1-based object slots; index 0 is unused so handle `0` is never valid.
    objects: Vec<Option<std::rc::Rc<std::cell::RefCell<crate::JsObject>>>>,
    /// 1-based array slots; index 0 is unused so handle `0` is never valid.
    arrays: Vec<Option<std::rc::Rc<std::cell::RefCell<crate::value::ArrayStorage>>>>,
    /// 1-based function slots; index 0 is unused so handle `0` is never valid.
    functions: Vec<Option<crate::value::JsFunction>>,
}

impl PageArena {
    fn new() -> Self {
        Self {
            epoch: 1,
            chunks: Vec::new(),
            current: Vec::new(),
            slots: vec![PageString {
                handle: 0,
                epoch: 0,
                ptr: std::ptr::null(),
                len: 0,
                _thread: PhantomData,
            }],
            intern: HashMap::new(),
            allocated: 0,
            objects: vec![None],
            arrays: vec![None],
            functions: vec![None],
        }
    }

    fn intern(&mut self, s: &str) -> PageString {
        let hash = hash_str(s);
        if let Some(handles) = self.intern.get(&hash) {
            for &handle in handles {
                let slot = self.slots[handle as usize];
                if slot.len as usize == s.len() && slot.as_str() == s {
                    return slot;
                }
            }
        }
        let ptr = self.alloc_bytes(s.as_bytes());
        let handle = self.slots.len() as u32;
        let interned = PageString {
            handle,
            epoch: self.epoch,
            ptr,
            len: s.len() as u32,
            _thread: PhantomData,
        };
        self.slots.push(interned);
        self.intern.entry(hash).or_default().push(handle);
        interned
    }

    fn alloc_bytes(&mut self, bytes: &[u8]) -> *const u8 {
        if bytes.is_empty() {
            return "".as_ptr();
        }
        let remaining = self.current.capacity().saturating_sub(self.current.len());
        if remaining < bytes.len() {
            let cap = self
                .current
                .capacity()
                .max(CHUNK)
                .saturating_mul(2)
                .max(bytes.len());
            let old = std::mem::replace(&mut self.current, Vec::with_capacity(cap));
            if !old.is_empty() {
                self.chunks.push(old);
            }
        }
        let start = self.current.len();
        self.current.extend_from_slice(bytes);
        self.allocated = self.allocated.saturating_add(bytes.len());
        // Safety: `current` is never reallocated after bytes are handed out;
        // a too-small chunk is retired into `chunks` instead of growing.
        unsafe { self.current.as_ptr().add(start) }
    }

    fn reset(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.epoch = 1;
        }
        self.chunks.clear();
        self.chunks.shrink_to_fit();
        self.current = Vec::new();
        self.slots.clear();
        self.slots.push(PageString {
            handle: 0,
            epoch: 0,
            ptr: std::ptr::null(),
            len: 0,
            _thread: PhantomData,
        });
        self.intern.clear();
        self.intern.shrink_to_fit();
        self.allocated = 0;
        self.objects.clear();
        self.objects.push(None);
        self.objects.shrink_to_fit();
        self.arrays.clear();
        self.arrays.push(None);
        self.arrays.shrink_to_fit();
        self.functions.clear();
        self.functions.push(None);
        self.functions.shrink_to_fit();
    }

    fn alloc_object(
        &mut self,
        object: std::rc::Rc<std::cell::RefCell<crate::JsObject>>,
    ) -> u32 {
        let handle = self.objects.len() as u32;
        self.objects.push(Some(object));
        handle
    }

    fn get_object(
        &self,
        handle: u32,
    ) -> std::rc::Rc<std::cell::RefCell<crate::JsObject>> {
        let Some(Some(object)) = self.objects.get(handle as usize) else {
            panic!("page object handle used after reset_bridge");
        };
        object.clone()
    }

    fn alloc_array(
        &mut self,
        array: std::rc::Rc<std::cell::RefCell<crate::value::ArrayStorage>>,
    ) -> u32 {
        let handle = self.arrays.len() as u32;
        self.arrays.push(Some(array));
        handle
    }

    fn get_array(
        &self,
        handle: u32,
    ) -> std::rc::Rc<std::cell::RefCell<crate::value::ArrayStorage>> {
        let Some(Some(array)) = self.arrays.get(handle as usize) else {
            panic!("page array handle used after reset_bridge");
        };
        array.clone()
    }

    fn alloc_function(&mut self, function: crate::value::JsFunction) -> u32 {
        let handle = self.functions.len() as u32;
        self.functions.push(Some(function));
        handle
    }

    fn get_function(&self, handle: u32) -> crate::value::JsFunction {
        let Some(Some(function)) = self.functions.get(handle as usize) else {
            panic!("page function handle used after reset_bridge");
        };
        function.clone()
    }
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

thread_local! {
    static ARENA: RefCell<PageArena> = RefCell::new(PageArena::new());
    static GEN: Cell<u32> = const { Cell::new(1) };
}

fn current_epoch() -> u32 {
    GEN.get()
}

fn sync_epoch(epoch: u32) {
    GEN.set(epoch);
}

/// Intern `s` into the current page bump. Clones of the returned handle do
/// not `Rc::clone`.
pub(crate) fn intern(s: &str) -> PageString {
    ARENA.with(|arena| {
        let interned = arena.borrow_mut().intern(s);
        sync_epoch(interned.epoch);
        interned
    })
}

/// Resolve a live intern handle. Panics if the handle belongs to a
/// previous page (same contract as [`PageString::as_str`]).
pub(crate) fn get(handle: u32) -> Option<PageString> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| {
        let arena = arena.borrow();
        let Some(slot) = arena.slots.get(handle as usize).copied() else {
            panic!("page-interned string used after reset_bridge");
        };
        if slot.handle != handle || slot.epoch != arena.epoch {
            panic!("page-interned string used after reset_bridge");
        }
        Some(slot)
    })
}

/// Drop interned page strings. Called from `reset_bridge` / navigation.
pub fn reset() {
    ARENA.with(|arena| {
        let mut arena = arena.borrow_mut();
        arena.reset();
        sync_epoch(arena.epoch);
    });
}

/// UTF-8 bytes currently interned in the page bump.
pub fn allocated_bytes() -> usize {
    ARENA.with(|arena| arena.borrow().allocated)
}

/// Live intern handles (empty after [`reset`]).
pub fn live_handles() -> usize {
    ARENA.with(|arena| arena.borrow().slots.len().saturating_sub(1))
}

/// Store a page-local object. Clone of the returned handle does not
/// `Rc::clone`.
pub(crate) fn alloc_object(
    object: std::rc::Rc<std::cell::RefCell<crate::JsObject>>,
) -> u32 {
    ARENA.with(|arena| arena.borrow_mut().alloc_object(object))
}

/// Resolve a live object handle. Panics if the handle belongs to a
/// previous page (same contract as interned strings).
pub(crate) fn get_object(
    handle: u32,
) -> std::rc::Rc<std::cell::RefCell<crate::JsObject>> {
    if handle == 0 {
        panic!("page object handle used after reset_bridge");
    }
    ARENA.with(|arena| arena.borrow().get_object(handle))
}

/// Live page-local objects (empty after [`reset`]).
pub fn live_objects() -> usize {
    ARENA.with(|arena| arena.borrow().objects.len().saturating_sub(1))
}

/// Store a page-local array. Clone of the returned handle does not
/// `Rc::clone`.
pub(crate) fn alloc_array(
    array: std::rc::Rc<std::cell::RefCell<crate::value::ArrayStorage>>,
) -> u32 {
    ARENA.with(|arena| arena.borrow_mut().alloc_array(array))
}

/// Resolve a live array handle. Panics if the handle belongs to a
/// previous page (same contract as interned strings).
pub(crate) fn get_array(
    handle: u32,
) -> std::rc::Rc<std::cell::RefCell<crate::value::ArrayStorage>> {
    if handle == 0 {
        panic!("page array handle used after reset_bridge");
    }
    ARENA.with(|arena| arena.borrow().get_array(handle))
}

/// Live page-local arrays (empty after [`reset`]).
pub fn live_arrays() -> usize {
    ARENA.with(|arena| arena.borrow().arrays.len().saturating_sub(1))
}

/// Store a page-local JS function. Clone of the returned handle does not
/// `Rc::clone` the closure / props / allocation.
pub(crate) fn alloc_function(function: crate::value::JsFunction) -> u32 {
    ARENA.with(|arena| arena.borrow_mut().alloc_function(function))
}

/// Resolve a live function handle. Panics if the handle belongs to a
/// previous page (same contract as interned strings).
pub(crate) fn get_function(handle: u32) -> crate::value::JsFunction {
    if handle == 0 {
        panic!("page function handle used after reset_bridge");
    }
    ARENA.with(|arena| arena.borrow().get_function(handle))
}

/// Live page-local functions (empty after [`reset`]).
pub fn live_functions() -> usize {
    ARENA.with(|arena| arena.borrow().functions.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_reuses_handle_and_reset_empties_table() {
        reset();
        assert_eq!(allocated_bytes(), 0);
        assert_eq!(live_handles(), 0);
        assert_eq!(live_objects(), 0);
        assert_eq!(live_arrays(), 0);
        assert_eq!(live_functions(), 0);

        let a = intern("page-arena-unique-key");
        let b = intern("page-arena-unique-key");
        assert_eq!(a.handle, b.handle);
        assert_eq!(a.epoch, b.epoch);
        assert_eq!(a.as_str(), "page-arena-unique-key");
        assert!(allocated_bytes() >= "page-arena-unique-key".len());
        assert_eq!(live_handles(), 1);

        let copied = a;
        assert_eq!(copied.handle, a.handle);
        assert_eq!(live_handles(), 1);

        intern("page-arena-other-key");
        assert_eq!(live_handles(), 2);

        reset();
        assert_eq!(allocated_bytes(), 0);
        assert_eq!(live_handles(), 0);
        assert_eq!(live_objects(), 0);
        assert_eq!(live_arrays(), 0);
        assert_eq!(live_functions(), 0);

        let again = intern("page-arena-unique-key");
        assert_eq!(again.as_str(), "page-arena-unique-key");
        assert_ne!(again.epoch, a.epoch);
    }

    #[test]
    fn object_table_alloc_and_reset_empties() {
        reset();
        assert_eq!(live_objects(), 0);
        let rc = std::rc::Rc::new(std::cell::RefCell::new(crate::JsObject::new()));
        let handle = alloc_object(rc.clone());
        assert_eq!(handle, 1);
        assert_eq!(std::rc::Rc::strong_count(&rc), 2);
        let resolved = get_object(handle);
        assert!(std::rc::Rc::ptr_eq(&rc, &resolved));
        drop(resolved);
        assert_eq!(live_objects(), 1);
        reset();
        assert_eq!(live_objects(), 0);
        assert_eq!(std::rc::Rc::strong_count(&rc), 1);
    }

    #[test]
    fn array_table_alloc_and_reset_empties() {
        reset();
        assert_eq!(live_arrays(), 0);
        let rc = std::rc::Rc::new(std::cell::RefCell::new(
            crate::value::ArrayStorage::new(Vec::new()),
        ));
        let handle = alloc_array(rc.clone());
        assert_eq!(handle, 1);
        assert_eq!(std::rc::Rc::strong_count(&rc), 2);
        let resolved = get_array(handle);
        assert!(std::rc::Rc::ptr_eq(&rc, &resolved));
        drop(resolved);
        assert_eq!(live_arrays(), 1);
        reset();
        assert_eq!(live_arrays(), 0);
        assert_eq!(std::rc::Rc::strong_count(&rc), 1);
    }

    #[test]
    fn function_table_alloc_and_reset_empties() {
        reset();
        assert_eq!(live_functions(), 0);
        let js = crate::value::JsFunction::new(|_, _| crate::Value::Undefined);
        let before = js.strong_counts();
        let handle = alloc_function(js.clone());
        assert_eq!(handle, 1);
        let after_alloc = js.strong_counts();
        assert_eq!(after_alloc.0, before.0 + 1);
        assert_eq!(after_alloc.1, before.1 + 1);
        assert_eq!(after_alloc.2, before.2 + 1);
        let resolved = get_function(handle);
        assert!(resolved.ptr_eq(&js));
        drop(resolved);
        assert_eq!(live_functions(), 1);
        reset();
        assert_eq!(live_functions(), 0);
        let after_reset = js.strong_counts();
        assert_eq!(after_reset, before);
    }
}
