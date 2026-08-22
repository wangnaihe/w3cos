//! Page-scoped bump arena, interned string handles, and JS object/array/function slots.
//!
//! Short interned JS strings, page-local [`crate::JsObject`]s, constructor
//! / literal arrays, and ordinary JS closures created via [`crate::Value::function`]
//! live here so clones copy a `u32` handle instead of `Rc::clone`. The bump
//! and slabs are dropped on [`reset`] (`reset_bridge` / document navigation).
//! Host / DOM wrappers keep the `Value::Object(Rc)` path so WeakRef intern
//! can drop unreferenced wrappers without a page reset. `array_hole` and host
//! arrays keep `Value::Array(Rc)` when they must be collectable without a
//! page reset. Host/DOM callables (`Value::callable`, jsdom call slots) stay
//! on the `Value::Function(Rc<FunctionData>)` / object call-slot `Rc` path.
//! Interned functions store one `FunctionData` in a size-class slab;
//! interned objects / arrays store one `RefCell<_>` in a size-class slab.
//! `as_function` / `get_function` / clone return a handle and do not clone
//! inner Rcs. `as_object` / `as_array` / `get_object` / `get_array` / clone
//! return a handle and do not clone the slab slot.
//!
//! Immediate handles are `Copy` and do not refcount, so dead interned
//! payloads cannot be reclaimed mid-page. Slabs only speed the alloc path:
//! many same-size `FunctionData` / `JsObject` / `ArrayStorage` payloads
//! share a chunk instead of one `Box` malloc each. Short strings stay on
//! the packed bump — intern never aborts after the copy, so size-class
//! padding would not reclaim holes.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
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
            // Leftover keys/values after reset_bridge: treat as dead, like
            // a failed Weak upgrade. Do not follow the retired bump pointer.
            return "";
        }
        // Safety: `ptr`/`len` name UTF-8 bytes in a bump chunk that lives
        // until `reset` bumps the epoch. Callers must not hold the `&str` across
        // a page reset.
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len as usize))
        }
    }
}

/// Same-size payload slab. One malloc per chunk; bump-allocate slots;
/// pointers stay stable (chunks never reallocate). Never frees mid-page.
/// `reset` / `Drop` drop every initialized slot, then the chunks.
///
/// Measured 64-bit classes:
/// - `RefCell<JsObject>` = 480 → 8 slots / 4K chunk (compact PropertyMap)
/// - `RefCell<ArrayStorage>` = 56 → 73 slots / 4K chunk
/// - `FunctionData` = 64 → 64 slots / 4K chunk (compact PropertyMap)
struct SizeClassSlab<T> {
    chunks: Vec<Box<[MaybeUninit<T>]>>,
    len: usize,
}

impl<T> SizeClassSlab<T> {
    const CHUNK_BYTES: usize = CHUNK;

    const fn slots_per_chunk() -> usize {
        let size = std::mem::size_of::<T>();
        let size = if size == 0 { 1 } else { size };
        let n = Self::CHUNK_BYTES / size;
        if n < 8 {
            8
        } else {
            n
        }
    }

    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    fn new_chunk() -> Box<[MaybeUninit<T>]> {
        let slots = Self::slots_per_chunk();
        let mut chunk = Vec::with_capacity(slots);
        chunk.resize_with(slots, MaybeUninit::uninit);
        chunk.into_boxed_slice()
    }

    #[inline]
    fn alloc(&mut self, value: T) -> *const T {
        let idx = self.len;
        let slots = Self::slots_per_chunk();
        let chunk_idx = idx / slots;
        let slot = idx % slots;
        if chunk_idx == self.chunks.len() {
            self.chunks.push(Self::new_chunk());
        }
        let ptr = self.chunks[chunk_idx][slot].write(value) as *const T;
        self.len += 1;
        ptr
    }

    fn get(&self, idx: usize) -> Option<*const T> {
        if idx >= self.len {
            return None;
        }
        let slots = Self::slots_per_chunk();
        Some(self.chunks[idx / slots][idx % slots].as_ptr())
    }

    fn len(&self) -> usize {
        self.len
    }

    fn reserved_bytes(&self) -> usize {
        self.chunks
            .len()
            .saturating_mul(Self::slots_per_chunk())
            .saturating_mul(std::mem::size_of::<T>())
    }

    fn clear(&mut self) {
        let slots = Self::slots_per_chunk();
        let mut remaining = self.len;
        self.len = 0;
        for chunk in self.chunks.iter_mut() {
            let n = remaining.min(slots);
            for slot in 0..n {
                // Safety: the first `len` slots were written by `alloc`.
                unsafe {
                    chunk[slot].assume_init_drop();
                }
            }
            remaining = remaining.saturating_sub(n);
        }
        self.chunks.clear();
        self.chunks.shrink_to_fit();
    }
}

impl<T> Drop for SizeClassSlab<T> {
    fn drop(&mut self) {
        self.clear();
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
    objects: SizeClassSlab<RefCell<crate::JsObject>>,
    arrays: SizeClassSlab<RefCell<crate::value::ArrayStorage>>,
    functions: SizeClassSlab<crate::value::FunctionData>,
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
            objects: SizeClassSlab::new(),
            arrays: SizeClassSlab::new(),
            functions: SizeClassSlab::new(),
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
        self.arrays.clear();
        self.functions.clear();
    }

    fn alloc_object(&mut self, object: crate::JsObject) -> u32 {
        self.objects.alloc(RefCell::new(object));
        self.objects.len() as u32
    }

    fn get_object(&self, handle: u32) -> Option<crate::value::JsObjectRef> {
        let idx = handle.checked_sub(1)? as usize;
        let ptr = self.objects.get(idx)?;
        Some(crate::value::JsObjectRef::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn upgrade_object(&self, handle: u32, epoch: u32) -> Option<crate::value::JsObjectRef> {
        if epoch != self.epoch {
            return None;
        }
        self.get_object(handle)
    }

    fn alloc_array(&mut self, array: crate::value::ArrayStorage) -> u32 {
        self.arrays.alloc(RefCell::new(array));
        self.arrays.len() as u32
    }

    fn get_array(&self, handle: u32) -> Option<crate::value::JsArrayRef> {
        let idx = handle.checked_sub(1)? as usize;
        let ptr = self.arrays.get(idx)?;
        Some(crate::value::JsArrayRef::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn upgrade_array(&self, handle: u32, epoch: u32) -> Option<crate::value::JsArrayRef> {
        if epoch != self.epoch {
            return None;
        }
        self.get_array(handle)
    }

    fn alloc_function(&mut self, function: crate::value::FunctionData) -> u32 {
        self.functions.alloc(function);
        self.functions.len() as u32
    }

    fn get_function(&self, handle: u32) -> Option<crate::value::JsFunction> {
        let idx = handle.checked_sub(1)? as usize;
        let ptr = self.functions.get(idx)?;
        Some(crate::value::JsFunction::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn upgrade_function(&self, handle: u32, epoch: u32) -> Option<crate::value::JsFunction> {
        if epoch != self.epoch {
            return None;
        }
        self.get_function(handle)
    }

    fn slab_bytes(&self) -> usize {
        self.objects
            .reserved_bytes()
            .saturating_add(self.arrays.reserved_bytes())
            .saturating_add(self.functions.reserved_bytes())
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

pub(crate) fn current_epoch() -> u32 {
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

/// Resolve a live intern handle. Returns `None` if the handle belongs
/// to a previous page (same dead-handle contract as interned objects).
pub(crate) fn get(handle: u32) -> Option<PageString> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| {
        let arena = arena.borrow();
        let slot = arena.slots.get(handle as usize).copied()?;
        if slot.handle != handle || slot.epoch != arena.epoch {
            return None;
        }
        Some(slot)
    })
}

/// Drop interned page strings and payload slabs. Called from `reset_bridge`
/// / navigation.
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

/// Reserved bytes in object / array / function size-class slabs.
pub fn slab_bytes() -> usize {
    ARENA.with(|arena| arena.borrow().slab_bytes())
}

/// Live intern handles (empty after [`reset`]).
pub fn live_handles() -> usize {
    ARENA.with(|arena| arena.borrow().slots.len().saturating_sub(1))
}

/// Store a page-local object. The arena owns the payload once;
/// returned handles are `Clone` (`u32` + cached slot pointer).
pub(crate) fn alloc_object(object: crate::JsObject) -> u32 {
    ARENA.with(|arena| arena.borrow_mut().alloc_object(object))
}

/// Resolve a live object handle. Returns `None` if the slot is empty
/// (previous-page leftover after [`reset`]). Does not clone the payload.
/// Already-resolved `JsObjectRef` values still epoch-check on deref.
pub(crate) fn get_object(handle: u32) -> Option<crate::value::JsObjectRef> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| arena.borrow().get_object(handle))
}

/// Upgrade an interned weak object. Fails after [`reset`] via epoch
/// or empty slot — not `Weak::upgrade` of an inner Rc.
pub(crate) fn upgrade_object(handle: u32, epoch: u32) -> Option<crate::value::JsObjectRef> {
    if handle == 0 || epoch != current_epoch() {
        return None;
    }
    ARENA.with(|arena| arena.borrow().upgrade_object(handle, epoch))
}

/// Live page-local objects (empty after [`reset`]).
pub fn live_objects() -> usize {
    ARENA.with(|arena| arena.borrow().objects.len())
}

/// Store a page-local array. The arena owns the payload once;
/// returned handles are `Clone` (`u32` + cached slot pointer).
pub(crate) fn alloc_array(array: crate::value::ArrayStorage) -> u32 {
    ARENA.with(|arena| arena.borrow_mut().alloc_array(array))
}

/// Resolve a live array handle. Returns `None` if the slot is empty
/// (previous-page leftover after [`reset`]). Does not clone the payload.
/// Already-resolved `JsArrayRef` values still epoch-check on deref.
pub(crate) fn get_array(handle: u32) -> Option<crate::value::JsArrayRef> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| arena.borrow().get_array(handle))
}

/// Upgrade an interned weak array. Fails after [`reset`] via epoch
/// or empty slot — not `Weak::upgrade` of an inner Rc.
pub(crate) fn upgrade_array(handle: u32, epoch: u32) -> Option<crate::value::JsArrayRef> {
    if handle == 0 || epoch != current_epoch() {
        return None;
    }
    ARENA.with(|arena| arena.borrow().upgrade_array(handle, epoch))
}

/// Live page-local arrays (empty after [`reset`]).
pub fn live_arrays() -> usize {
    ARENA.with(|arena| arena.borrow().arrays.len())
}

/// Store a page-local JS function. The arena owns the payload once;
/// returned handles are `Copy` (`u32` + cached slot pointer).
pub(crate) fn alloc_function(function: crate::value::FunctionData) -> u32 {
    ARENA.with(|arena| arena.borrow_mut().alloc_function(function))
}

/// Resolve a live function handle. Returns `None` if the slot is empty
/// (previous-page leftover after [`reset`]). Does not clone the payload.
/// Already-resolved `JsFunction` values still epoch-check on deref.
pub(crate) fn get_function(handle: u32) -> Option<crate::value::JsFunction> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| arena.borrow().get_function(handle))
}

/// Upgrade an interned weak function. Fails after [`reset`] via epoch
/// or empty slot — not `Weak::upgrade` of inner Rcs.
pub(crate) fn upgrade_function(handle: u32, epoch: u32) -> Option<crate::value::JsFunction> {
    if handle == 0 || epoch != current_epoch() {
        return None;
    }
    ARENA.with(|arena| arena.borrow().upgrade_function(handle, epoch))
}

/// Live page-local functions (empty after [`reset`]).
pub fn live_functions() -> usize {
    ARENA.with(|arena| arena.borrow().functions.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_empty_page() {
        assert_eq!(allocated_bytes(), 0);
        assert_eq!(slab_bytes(), 0);
        assert_eq!(live_handles(), 0);
        assert_eq!(live_objects(), 0);
        assert_eq!(live_arrays(), 0);
        assert_eq!(live_functions(), 0);
    }

    #[test]
    fn intern_reuses_handle_and_reset_empties_table() {
        reset();
        assert_empty_page();

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
        assert_empty_page();

        let again = intern("page-arena-unique-key");
        assert_eq!(again.as_str(), "page-arena-unique-key");
        assert_ne!(again.epoch, a.epoch);
    }

    #[test]
    fn object_table_alloc_and_reset_empties() {
        reset();
        assert_eq!(live_objects(), 0);
        assert_eq!(slab_bytes(), 0);
        let handle = alloc_object(crate::JsObject::new());
        assert_eq!(handle, 1);
        let resolved = get_object(handle).expect("live object");
        let cloned = resolved.clone();
        assert!(resolved.ptr_eq(&cloned));
        assert_eq!(resolved.interned_handle(), Some(handle));
        assert!(resolved.host_strong_count().is_none());
        assert_eq!(live_objects(), 1);
        assert!(slab_bytes() >= std::mem::size_of::<RefCell<crate::JsObject>>());
        drop(resolved);
        drop(cloned);
        assert_eq!(live_objects(), 1);
        reset();
        assert_eq!(live_objects(), 0);
        assert_eq!(slab_bytes(), 0);
    }

    #[test]
    fn array_table_alloc_and_reset_empties() {
        reset();
        assert_eq!(live_arrays(), 0);
        assert_eq!(slab_bytes(), 0);
        let handle = alloc_array(crate::value::ArrayStorage::new(Vec::new()));
        assert_eq!(handle, 1);
        let resolved = get_array(handle).expect("live array");
        let cloned = resolved.clone();
        assert!(resolved.ptr_eq(&cloned));
        assert_eq!(resolved.interned_handle(), Some(handle));
        assert!(resolved.host_strong_count().is_none());
        assert_eq!(live_arrays(), 1);
        assert!(slab_bytes() >= std::mem::size_of::<RefCell<crate::value::ArrayStorage>>());
        drop(resolved);
        drop(cloned);
        assert_eq!(live_arrays(), 1);
        reset();
        assert_eq!(live_arrays(), 0);
        assert_eq!(slab_bytes(), 0);
    }

    #[test]
    fn function_table_alloc_and_reset_empties() {
        reset();
        assert_eq!(live_functions(), 0);
        assert_eq!(slab_bytes(), 0);
        let handle = alloc_function(crate::value::FunctionData::new(|_, _| {
            crate::Value::Undefined
        }));
        assert_eq!(handle, 1);
        let resolved = get_function(handle).expect("live function");
        let cloned = resolved.clone();
        assert!(resolved.ptr_eq(&cloned));
        assert_eq!(resolved.interned_handle(), Some(handle));
        assert!(resolved.host_strong_count().is_none());
        assert_eq!(live_functions(), 1);
        // FunctionData::new also interns a prototype object.
        assert_eq!(live_objects(), 1);
        assert!(slab_bytes() > 0);
        drop(resolved);
        drop(cloned);
        assert_eq!(live_functions(), 1);
        reset();
        assert_eq!(live_functions(), 0);
        assert_eq!(live_objects(), 0);
        assert_eq!(slab_bytes(), 0);
    }

    #[test]
    fn reset_empties_slab_bytes_and_live_counts() {
        reset();
        assert_empty_page();

        let object_size = std::mem::size_of::<RefCell<crate::JsObject>>();
        let array_size = std::mem::size_of::<RefCell<crate::value::ArrayStorage>>();
        let function_size = std::mem::size_of::<crate::value::FunctionData>();
        assert!(object_size > 0 && array_size > 0 && function_size > 0);

        intern("slab-reset-key");
        let _o = alloc_object(crate::JsObject::new());
        let _a = alloc_array(crate::value::ArrayStorage::new(Vec::new()));
        let _f = alloc_function(crate::value::FunctionData::new(|_, _| {
            crate::Value::Undefined
        }));

        // FunctionData::new interns "prototype"; PropertyMap Inline placeholders
        // also intern "" (shared across empty slots).
        assert_eq!(live_handles(), 3);
        assert_eq!(live_objects(), 2); // explicit + function prototype
        assert_eq!(live_arrays(), 1);
        assert_eq!(live_functions(), 1);
        assert!(allocated_bytes() >= "slab-reset-key".len());
        assert!(slab_bytes() > 0);
        assert_eq!(
            slab_bytes(),
            SizeClassSlab::<RefCell<crate::JsObject>>::slots_per_chunk() * object_size
                + SizeClassSlab::<RefCell<crate::value::ArrayStorage>>::slots_per_chunk()
                    * array_size
                + SizeClassSlab::<crate::value::FunctionData>::slots_per_chunk() * function_size
        );

        reset();
        assert_empty_page();
    }

    #[test]
    fn same_size_payloads_share_one_slab_chunk() {
        reset();
        let fn_slots = SizeClassSlab::<crate::value::FunctionData>::slots_per_chunk();
        let obj_slots = SizeClassSlab::<RefCell<crate::JsObject>>::slots_per_chunk();
        let n = fn_slots.min(obj_slots).min(16);
        assert!(n >= 8);

        for _ in 0..n {
            alloc_function(crate::value::FunctionData::new(|_, _| {
                crate::Value::Undefined
            }));
        }
        assert_eq!(live_functions(), n);
        assert_eq!(live_objects(), n);

        let fn_chunk = fn_slots * std::mem::size_of::<crate::value::FunctionData>();
        let obj_chunk = obj_slots * std::mem::size_of::<RefCell<crate::JsObject>>();
        // n functions + n prototypes fit in one chunk each (not n Box mallocs).
        assert_eq!(slab_bytes(), fn_chunk + obj_chunk);

        reset();
        assert_eq!(slab_bytes(), 0);
        assert_eq!(live_functions(), 0);
        assert_eq!(live_objects(), 0);
    }
}
