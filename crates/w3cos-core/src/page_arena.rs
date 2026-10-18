//! Page-scoped bump arena, interned string handles, and JS object/array/function slots.
//!
//! Page-interned JS strings (any length), page-local [`crate::JsObject`]s, constructor
//! / literal arrays, and ordinary JS closures created via [`crate::Value::function`]
//! live here as one `u64` handle word whose clone bumps a slot count. The bump
//! and slabs are dropped on [`reset`] (`reset_bridge` / document navigation).
//! Host/outliving strings keep [`crate::JsString::heap`] (thin `Rc<String>`).
//! Host / DOM wrappers keep the `Value::Object(Rc)` path so WeakRef intern
//! can drop unreferenced wrappers without a page reset. `array_hole` and host
//! arrays keep `Value::Array(Rc)` when they must be collectable without a
//! page reset. Host/DOM callables (`Value::callable`, jsdom call slots) stay
//! on the `Value::Function(Rc<FunctionData>)` / object call-slot `Rc` path.
//! Interned functions store one `FunctionData` in a size-class slab;
//! interned objects / arrays store one `RefCell<_>` in a size-class slab.
//! `as_function` / `get_function` / clone return a counted handle and do not
//! clone inner Rcs. `as_object` / `as_array` / `get_object` / `get_array` /
//! clone return a counted handle and do not clone the slab slot.
//!
//! Object / array / function slots are strictly reference counted. A
//! `Value` immediate or a resolved `JsObjectRef` / `JsArrayRef` /
//! `JsFunction` handle owns one strong count ([`retain`] on clone,
//! [`release`] on drop). At zero the slot dies at once (weak references
//! stop upgrading), its payload is dropped from an iterative work list
//! outside the arena borrow, and the slot is reused from a free list with a
//! bumped generation. Cycles keep their counts and wait for [`reset`], the
//! bulk free. Many same-size payloads still share a chunk instead of one
//! `Box` malloc each. Strings stay on the packed bump and are not counted:
//! intern never aborts after the copy, so size-class padding would not
//! reclaim holes.

use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::hash_map::DefaultHasher;
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

/// Bits of an object / array / function handle that hold the 1-based slot
/// index. The top 8 bits carry the low byte of the slot generation, so a
/// handle to a freed-and-reused slot never equals the new occupant's handle
/// (until the byte wraps; weak references compare the full generation).
const INDEX_BITS: u32 = 24;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
/// A saturated strong count pins the slot until [`reset`].
const PINNED: u32 = u32::MAX;

#[inline]
fn encode_handle(idx: usize, generation: u32) -> u32 {
    (idx as u32 + 1) | ((generation & 0xFF) << INDEX_BITS)
}

#[inline]
fn decode_handle(handle: u32) -> Option<(usize, u32)> {
    let index = handle & INDEX_MASK;
    if index == 0 {
        return None;
    }
    Some(((index - 1) as usize, handle >> INDEX_BITS))
}

/// Per-slot bookkeeping, kept beside (not inside) the payload so a count
/// change never touches payload bytes.
///
/// Fields are `Cell`s: the count fast path ([`retain`] / [`release`])
/// updates them through the published [`FAST`] pointer without borrowing
/// the arena, so they must tolerate shared access.
#[derive(Default)]
struct SlotMeta {
    /// Strong references: `Value` handles plus resolved `JsObjectRef` /
    /// `JsArrayRef` / `JsFunction` handles. Weak references do not count.
    strong: Cell<u32>,
    /// Bumped when the strong count reaches zero. Weak references store the
    /// full value; handles store its low byte.
    generation: Cell<u32>,
    /// The payload is initialized (live, or dead but not yet dropped).
    init: Cell<bool>,
}

impl SlotMeta {
    /// Live (counted) and its generation byte matches the handle's.
    #[inline(always)]
    fn is_live(&self, generation_byte: u32) -> bool {
        self.strong.get() > 0 && (self.generation.get() & 0xFF) == generation_byte
    }
}

/// Where the current page's slot metadata for one [`SlotKind`] lives, so a
/// count change on a live handle needs no arena borrow and no lazily
/// initialized thread-local (both cost more than the count itself).
///
/// Invariants (upheld by [`SizeClassSlab`]):
/// - `ptr[..len]` is the `meta` buffer of the arena's *current* slab of that
///   kind. It is republished after every `meta.push` (the only operation
///   that can move the buffer) and cleared before a slab drops its payloads
///   or its buffer, so it never dangles.
/// - Only the `Cell` fields are touched through it, and never while a
///   `&mut SlotMeta` exists (slab code only uses `&SlotMeta`).
/// - Handles from a previous page never get here: `Immediate::arena_slot`
///   and the ref types check the epoch first.
struct FastMeta {
    ptr: Cell<*const SlotMeta>,
    len: Cell<usize>,
}

impl FastMeta {
    const EMPTY: Self = Self {
        ptr: Cell::new(std::ptr::null()),
        len: Cell::new(0),
    };

    /// Metadata of a live current-page slot, or `None` (stale generation,
    /// dead, or not published), in which case callers take the slow path.
    #[inline(always)]
    fn live(&self, handle: u32) -> Option<&SlotMeta> {
        let index = handle & INDEX_MASK;
        let idx = index.wrapping_sub(1) as usize;
        if idx >= self.len.get() {
            return None;
        }
        // Safety: `idx < len` and `ptr[..len]` is the current slab's live
        // meta buffer (see the type invariants).
        let meta = unsafe { &*self.ptr.get().add(idx) };
        meta.is_live(handle >> INDEX_BITS).then_some(meta)
    }
}

thread_local! {
    /// Indexed by `SlotKind as usize`. `const` and drop-free, so access is a
    /// plain TLS offset with no lazy-init or destructor-state check.
    static FAST: [FastMeta; 3] = const { [FastMeta::EMPTY, FastMeta::EMPTY, FastMeta::EMPTY] };
}

/// Same-size payload slab. One malloc per chunk; pointers stay stable
/// (chunks never reallocate).
///
/// Each slot is strictly reference counted: `alloc` hands out one strong
/// count, [`retain`] / [`release`] adjust it, and a slot whose count reaches
/// zero is dropped (iteratively, see [`schedule_drop`]) and pushed on a free
/// list for reuse with a bumped generation. Cycles keep their counts above
/// zero and stay until `reset`, which drops every initialized slot.
///
/// Measured 64-bit classes:
/// - `RefCell<JsObject>` = 304 → 13 slots / 4K chunk (lazy-boxed rare fields)
/// - `RefCell<ArrayStorage>` = 56 → 73 slots / 4K chunk
/// - `FunctionData` = 64 → 64 slots / 4K chunk (compact PropertyMap)
struct SizeClassSlab<T> {
    kind: SlotKind,
    chunks: Vec<Box<[UnsafeCell<MaybeUninit<T>>]>>,
    meta: Vec<SlotMeta>,
    free: Vec<u32>,
    live: usize,
}

impl<T> SizeClassSlab<T> {
    const CHUNK_BYTES: usize = CHUNK;

    const fn slots_per_chunk() -> usize {
        let size = std::mem::size_of::<T>();
        let size = if size == 0 { 1 } else { size };
        let n = Self::CHUNK_BYTES / size;
        if n < 8 { 8 } else { n }
    }

    fn new(kind: SlotKind) -> Self {
        let slab = Self {
            kind,
            chunks: Vec::new(),
            meta: Vec::new(),
            free: Vec::new(),
            live: 0,
        };
        // An empty slab: nothing to reach through the fast path.
        let _ = FAST.try_with(|fast| fast[kind as usize].len.set(0));
        slab
    }

    /// Point the count fast path at this slab's (possibly moved) meta buffer.
    #[inline]
    fn publish(&self) {
        let _ = FAST.try_with(|fast| {
            let fast = &fast[self.kind as usize];
            fast.ptr.set(self.meta.as_ptr());
            fast.len.set(self.meta.len());
        });
    }

    /// Stop the fast path from reaching this slab (before its payloads or
    /// buffer are dropped). A retired slab is no longer published, unless
    /// both buffers are empty, where `len` is already zero.
    fn unpublish(&self) {
        let _ = FAST.try_with(|fast| {
            let fast = &fast[self.kind as usize];
            if fast.ptr.get() == self.meta.as_ptr() {
                fast.len.set(0);
            }
        });
    }

    fn new_chunk() -> Box<[UnsafeCell<MaybeUninit<T>>]> {
        (0..Self::slots_per_chunk())
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect()
    }

    #[inline]
    fn slot_ptr(&self, idx: usize) -> *mut T {
        let slots = Self::slots_per_chunk();
        self.chunks[idx / slots][idx % slots].get().cast::<T>()
    }

    /// Store `value` with one strong count. `Err` when the handle space is
    /// exhausted; the caller then falls back to a host `Rc`.
    #[inline]
    fn alloc(&mut self, value: T) -> Result<u32, T> {
        let idx = match self.free.pop() {
            Some(idx) => idx as usize,
            None => {
                let idx = self.meta.len();
                if idx >= INDEX_MASK as usize {
                    return Err(value);
                }
                if idx / Self::slots_per_chunk() == self.chunks.len() {
                    self.chunks.push(Self::new_chunk());
                }
                self.meta.push(SlotMeta::default());
                self.publish();
                idx
            }
        };
        // Safety: `idx` is a free slot (never written, or dropped by
        // `begin_drop` and returned by `finish_drop`).
        unsafe { self.slot_ptr(idx).write(value) };
        let meta = &self.meta[idx];
        meta.strong.set(1);
        meta.init.set(true);
        self.live += 1;
        Ok(encode_handle(idx, meta.generation.get()))
    }

    /// Slot index of a live handle whose generation byte matches.
    #[inline]
    fn live_index(&self, handle: u32) -> Option<usize> {
        let (idx, generation) = decode_handle(handle)?;
        let meta = self.meta.get(idx)?;
        meta.is_live(generation).then_some(idx)
    }

    #[inline]
    fn get(&self, handle: u32) -> Option<*const T> {
        self.live_index(handle)
            .map(|idx| self.slot_ptr(idx) as *const T)
    }

    fn generation(&self, handle: u32) -> Option<u32> {
        self.live_index(handle)
            .map(|idx| self.meta[idx].generation.get())
    }

    fn upgrade(&self, handle: u32, generation: u32) -> Option<*const T> {
        let idx = self.live_index(handle)?;
        (self.meta[idx].generation.get() == generation).then(|| self.slot_ptr(idx) as *const T)
    }

    #[inline]
    fn retain(&mut self, handle: u32) -> bool {
        let Some(idx) = self.live_index(handle) else {
            return false;
        };
        let meta = &self.meta[idx];
        let strong = meta.strong.get();
        if strong != PINNED {
            meta.strong.set(strong + 1);
        }
        true
    }

    /// Drop one strong count. Returns the slot index when it reached zero;
    /// the slot is then dead to handles and weak refs (generation bumped)
    /// but its payload is still initialized until [`Self::begin_drop`].
    #[inline]
    fn release(&mut self, handle: u32) -> Option<usize> {
        let idx = self.live_index(handle)?;
        let meta = &self.meta[idx];
        let strong = meta.strong.get();
        if strong == PINNED {
            return None;
        }
        meta.strong.set(strong - 1);
        if strong > 1 {
            return None;
        }
        meta.generation.set(meta.generation.get().wrapping_add(1));
        self.live -= 1;
        Some(idx)
    }

    /// Hand out a dead slot's payload pointer for an in-place drop. The
    /// slot is not reusable until [`Self::finish_drop`].
    fn begin_drop(&mut self, idx: usize) -> Option<*mut T> {
        let meta = self.meta.get(idx)?;
        if meta.strong.get() != 0 || !meta.init.get() {
            return None;
        }
        meta.init.set(false);
        Some(self.slot_ptr(idx))
    }

    fn finish_drop(&mut self, idx: usize) {
        if let Some(meta) = self.meta.get(idx)
            && meta.strong.get() == 0
            && !meta.init.get()
        {
            self.free.push(idx as u32);
        }
    }

    /// Live (strongly referenced) slots.
    fn len(&self) -> usize {
        self.live
    }

    fn reserved_bytes(&self) -> usize {
        self.chunks
            .len()
            .saturating_mul(Self::slots_per_chunk())
            .saturating_mul(std::mem::size_of::<T>())
    }

    /// Drop every initialized slot (live, cycle-held, or dead but pending),
    /// then the chunks. Only called on a slab already detached from the
    /// arena, so payload drops may re-enter the (new) arena freely.
    fn clear(&mut self) {
        self.unpublish();
        for idx in 0..self.meta.len() {
            if !self.meta[idx].init.get() {
                continue;
            }
            self.meta[idx].init.set(false);
            // Safety: `init` slots hold a value written by `alloc` and not
            // yet dropped.
            unsafe { std::ptr::drop_in_place(self.slot_ptr(idx)) };
        }
        self.meta.clear();
        self.free.clear();
        self.live = 0;
        self.chunks.clear();
        self.chunks.shrink_to_fit();
    }
}

impl<T> Drop for SizeClassSlab<T> {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Which slab a handle names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub(crate) enum SlotKind {
    Object = 0,
    Array = 1,
    Function = 2,
}

/// Slabs detached by [`reset`]; dropping it drops the old page's payloads.
struct RetiredSlabs {
    _objects: SizeClassSlab<RefCell<crate::JsObject>>,
    _arrays: SizeClassSlab<RefCell<crate::value::ArrayStorage>>,
    _functions: SizeClassSlab<crate::value::FunctionData>,
}

enum DropPtr {
    Object(*mut RefCell<crate::JsObject>),
    Array(*mut RefCell<crate::value::ArrayStorage>),
    Function(*mut crate::value::FunctionData),
}

impl DropPtr {
    /// Safety: the pointer came from `begin_drop` and its chunk is alive.
    unsafe fn drop_payload(self) {
        unsafe {
            match self {
                DropPtr::Object(ptr) => std::ptr::drop_in_place(ptr),
                DropPtr::Array(ptr) => std::ptr::drop_in_place(ptr),
                DropPtr::Function(ptr) => std::ptr::drop_in_place(ptr),
            }
        }
    }
}

struct PageArena {
    epoch: u32,
    chunks: Vec<Vec<u8>>,
    current: Vec<u8>,
    /// 1-based intern slots; index 0 is unused so handle `0` is never valid.
    slots: Vec<PageString>,
    /// Open-addressed intern lookup. `0` is empty; otherwise a 1-based handle.
    hash_slots: Vec<u32>,
    hash_mask: usize,
    hash_len: usize,
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
            current: Vec::with_capacity(1 << 20),
            slots: {
                let mut slots = Vec::with_capacity(32768);
                slots.push(PageString {
                    handle: 0,
                    epoch: 0,
                    ptr: std::ptr::null(),
                    len: 0,
                    _thread: PhantomData,
                });
                slots
            },
            hash_slots: vec![0; 32768],
            hash_mask: 32767,
            hash_len: 0,
            allocated: 0,
            objects: SizeClassSlab::new(SlotKind::Object),
            arrays: SizeClassSlab::new(SlotKind::Array),
            functions: SizeClassSlab::new(SlotKind::Function),
        }
    }

    fn intern(&mut self, s: &str) -> PageString {
        let hash = hash_str(s);
        if let Some(existing) = self.hash_find(hash, s) {
            return existing;
        }
        if self.hash_slots.is_empty() || (self.hash_len + 1) * 10 >= self.hash_slots.len() * 7 {
            self.hash_grow();
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
        self.hash_insert(hash, handle);
        interned
    }

    fn hash_find(&self, hash: u64, s: &str) -> Option<PageString> {
        if self.hash_slots.is_empty() {
            return None;
        }
        let mut index = hash as usize & self.hash_mask;
        for _ in 0..self.hash_slots.len() {
            let handle = self.hash_slots[index];
            if handle == 0 {
                return None;
            }
            let slot = self.slots[handle as usize];
            if slot.len as usize == s.len() && slot.as_str() == s {
                return Some(slot);
            }
            index = (index + 1) & self.hash_mask;
        }
        None
    }

    fn hash_insert(&mut self, hash: u64, handle: u32) {
        let mut index = hash as usize & self.hash_mask;
        loop {
            if self.hash_slots[index] == 0 {
                self.hash_slots[index] = handle;
                self.hash_len += 1;
                return;
            }
            index = (index + 1) & self.hash_mask;
        }
    }

    fn hash_grow(&mut self) {
        let next = (self.hash_slots.len().max(1024)) * 2;
        let old = std::mem::replace(&mut self.hash_slots, vec![0; next]);
        self.hash_mask = next - 1;
        self.hash_len = 0;
        if old.is_empty() {
            return;
        }
        for handle in old {
            if handle == 0 {
                continue;
            }
            let slot = self.slots[handle as usize];
            let hash = hash_str(slot.as_str());
            self.hash_insert(hash, handle);
        }
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

    fn reset(&mut self) -> RetiredSlabs {
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
        self.hash_slots.fill(0);
        self.hash_len = 0;
        self.allocated = 0;
        RetiredSlabs {
            _objects: std::mem::replace(&mut self.objects, SizeClassSlab::new(SlotKind::Object)),
            _arrays: std::mem::replace(&mut self.arrays, SizeClassSlab::new(SlotKind::Array)),
            _functions: std::mem::replace(
                &mut self.functions,
                SizeClassSlab::new(SlotKind::Function),
            ),
        }
    }

    fn alloc_object(&mut self, object: crate::JsObject) -> Result<u32, crate::JsObject> {
        self.objects
            .alloc(RefCell::new(object))
            .map_err(RefCell::into_inner)
    }

    /// Resolve a live handle and take one strong count for the returned ref.
    fn get_object(&mut self, handle: u32) -> Option<crate::value::JsObjectRef> {
        let ptr = self.objects.get(handle)?;
        self.objects.retain(handle);
        Some(crate::value::JsObjectRef::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn upgrade_object(
        &mut self,
        handle: u32,
        epoch: u32,
        generation: u32,
    ) -> Option<crate::value::JsObjectRef> {
        if epoch != self.epoch {
            return None;
        }
        let ptr = self.objects.upgrade(handle, generation)?;
        self.objects.retain(handle);
        Some(crate::value::JsObjectRef::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn alloc_array(
        &mut self,
        array: crate::value::ArrayStorage,
    ) -> Result<u32, crate::value::ArrayStorage> {
        self.arrays
            .alloc(RefCell::new(array))
            .map_err(RefCell::into_inner)
    }

    fn get_array(&mut self, handle: u32) -> Option<crate::value::JsArrayRef> {
        let ptr = self.arrays.get(handle)?;
        self.arrays.retain(handle);
        Some(crate::value::JsArrayRef::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn upgrade_array(
        &mut self,
        handle: u32,
        epoch: u32,
        generation: u32,
    ) -> Option<crate::value::JsArrayRef> {
        if epoch != self.epoch {
            return None;
        }
        let ptr = self.arrays.upgrade(handle, generation)?;
        self.arrays.retain(handle);
        Some(crate::value::JsArrayRef::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn alloc_function(
        &mut self,
        function: crate::value::FunctionData,
    ) -> Result<u32, crate::value::FunctionData> {
        self.functions.alloc(function)
    }

    fn get_function(&mut self, handle: u32) -> Option<crate::value::JsFunction> {
        let ptr = self.functions.get(handle)?;
        self.functions.retain(handle);
        Some(crate::value::JsFunction::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    fn upgrade_function(
        &mut self,
        handle: u32,
        epoch: u32,
        generation: u32,
    ) -> Option<crate::value::JsFunction> {
        if epoch != self.epoch {
            return None;
        }
        let ptr = self.functions.upgrade(handle, generation)?;
        self.functions.retain(handle);
        Some(crate::value::JsFunction::from_interned(
            handle, self.epoch, ptr,
        ))
    }

    #[inline]
    fn retain(&mut self, kind: SlotKind, handle: u32) -> bool {
        match kind {
            SlotKind::Object => self.objects.retain(handle),
            SlotKind::Array => self.arrays.retain(handle),
            SlotKind::Function => self.functions.retain(handle),
        }
    }

    #[inline]
    fn release(&mut self, kind: SlotKind, handle: u32) -> Option<usize> {
        match kind {
            SlotKind::Object => self.objects.release(handle),
            SlotKind::Array => self.arrays.release(handle),
            SlotKind::Function => self.functions.release(handle),
        }
    }

    fn generation(&self, kind: SlotKind, handle: u32) -> Option<u32> {
        match kind {
            SlotKind::Object => self.objects.generation(handle),
            SlotKind::Array => self.arrays.generation(handle),
            SlotKind::Function => self.functions.generation(handle),
        }
    }

    fn begin_drop(&mut self, kind: SlotKind, idx: usize) -> Option<DropPtr> {
        match kind {
            SlotKind::Object => self.objects.begin_drop(idx).map(DropPtr::Object),
            SlotKind::Array => self.arrays.begin_drop(idx).map(DropPtr::Array),
            SlotKind::Function => self.functions.begin_drop(idx).map(DropPtr::Function),
        }
    }

    fn finish_drop(&mut self, kind: SlotKind, idx: usize) {
        match kind {
            SlotKind::Object => self.objects.finish_drop(idx),
            SlotKind::Array => self.arrays.finish_drop(idx),
            SlotKind::Function => self.functions.finish_drop(idx),
        }
    }

    #[cfg(test)]
    fn strong_count(&self, kind: SlotKind, handle: u32) -> Option<u32> {
        let meta = |slab_meta: &[SlotMeta], idx: usize| slab_meta[idx].strong.get();
        match kind {
            SlotKind::Object => self
                .objects
                .live_index(handle)
                .map(|idx| meta(&self.objects.meta, idx)),
            SlotKind::Array => self
                .arrays
                .live_index(handle)
                .map(|idx| meta(&self.arrays.meta, idx)),
            SlotKind::Function => self
                .functions
                .live_index(handle)
                .map(|idx| meta(&self.functions.meta, idx)),
        }
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

/// UTF-8 for a page-intern handle. Stale epoch / missing slot yields `""`
/// (no panic), matching leftover Values after [`reset`].
///
/// The slice is valid until the next [`reset`] — same contract as
/// [`PageString::as_str`]. `'static` is a type-system lie for that window.
pub(crate) fn str_at(handle: u32, epoch: u32) -> &'static str {
    match get(handle) {
        Some(slot) if slot.epoch == epoch => {
            let s = slot.as_str();
            // Safety: bump bytes live until `reset`; see `PageString::as_str`.
            unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(s.as_ptr(), s.len()))
            }
        }
        _ => "",
    }
}

/// Drop interned page strings and payload slabs. Called from `reset_bridge`
/// / navigation.
///
/// The epoch is bumped before any payload is dropped, so handles released
/// by those drops are stale no-ops. The old slabs are dropped outside the
/// arena borrow; inside a running [`schedule_drop`] they are parked until
/// the in-place drop in progress has finished with its chunk.
pub fn reset() {
    #[cfg(feature = "leak-detector")]
    {
        let _ = crate::leak_detector::report_from_env("page reset");
        crate::leak_detector::clear_baseline();
    }
    let retired = ARENA.with(|arena| {
        let mut arena = arena.borrow_mut();
        let retired = arena.reset();
        sync_epoch(arena.epoch);
        retired
    });
    if DRAINING.get() {
        let parked = GRAVEYARD.try_with(|graveyard| graveyard.borrow_mut().push(retired));
        if let Err(_) = parked {
            // Thread teardown: nothing else can touch the old slabs.
        }
        return;
    }
    drop(retired);
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

thread_local! {
    /// Dead slots whose payloads still have to be dropped: `(kind, slot
    /// index, epoch)`. A LIFO work list instead of recursion, so releasing
    /// a long chain or a huge array of arena values runs in constant stack.
    static PENDING: RefCell<Vec<(SlotKind, u32, u32)>> = const { RefCell::new(Vec::new()) };
    static DRAINING: Cell<bool> = const { Cell::new(false) };
    /// Slabs retired by a `reset` that ran inside a payload drop.
    static GRAVEYARD: RefCell<Vec<RetiredSlabs>> = const { RefCell::new(Vec::new()) };
}

/// Take one more strong count on a live handle (`Value` / ref clone).
///
/// Panics if the arena is mutably borrowed: skipping the increment would
/// let a later release free a slot that is still referenced.
#[inline(always)]
pub(crate) fn retain(kind: SlotKind, handle: u32) {
    #[cfg(feature = "rc-stats")]
    RC_OPS.with(|ops| ops[0].set(ops[0].get() + 1));
    let fast = FAST.try_with(|fast| match fast[kind as usize].live(handle) {
        Some(meta) => {
            let strong = meta.strong.get();
            if strong != PINNED {
                meta.strong.set(strong + 1);
            }
            true
        }
        None => false,
    });
    if fast != Ok(true) {
        retain_slow(kind, handle);
    }
}

#[cold]
#[inline(never)]
fn retain_slow(kind: SlotKind, handle: u32) {
    let _ = ARENA.try_with(|arena| arena.borrow_mut().retain(kind, handle));
}

/// Drop one strong count. At zero the slot dies: weak references stop
/// upgrading at once, and the payload (with every child it holds) is
/// dropped through the iterative [`schedule_drop`] queue before this
/// returns, unless an outer drop is already draining it.
///
/// A release that cannot reach the arena (thread teardown, or an arena
/// borrow that should not exist) leaks the slot until reset rather than
/// risking a free of a referenced slot.
#[inline(always)]
pub(crate) fn release(kind: SlotKind, handle: u32) {
    #[cfg(feature = "rc-stats")]
    RC_OPS.with(|ops| ops[1].set(ops[1].get() + 1));
    // Fast path: a live slot that keeps at least one count. The zero
    // transition (generation bump, live count, payload drop) stays on the
    // slow path under the arena borrow.
    let fast = FAST.try_with(|fast| match fast[kind as usize].live(handle) {
        Some(meta) => {
            let strong = meta.strong.get();
            if strong > 1 && strong != PINNED {
                meta.strong.set(strong - 1);
                true
            } else {
                false
            }
        }
        None => false,
    });
    if fast != Ok(true) {
        release_slow(kind, handle);
    }
}

#[cold]
#[inline(never)]
fn release_slow(kind: SlotKind, handle: u32) {
    let freed = ARENA
        .try_with(|arena| {
            arena
                .try_borrow_mut()
                .ok()
                .and_then(|mut arena| arena.release(kind, handle))
        })
        .ok()
        .flatten();
    if let Some(idx) = freed {
        schedule_drop(kind, idx as u32);
    }
}

struct DrainGuard;

impl Drop for DrainGuard {
    fn drop(&mut self) {
        DRAINING.set(false);
    }
}

fn schedule_drop(kind: SlotKind, idx: u32) {
    let epoch = current_epoch();
    if PENDING
        .try_with(|pending| pending.borrow_mut().push((kind, idx, epoch)))
        .is_err()
    {
        return;
    }
    if DRAINING.get() {
        return;
    }
    DRAINING.set(true);
    let guard = DrainGuard;
    loop {
        let Some((kind, idx, epoch)) = PENDING.with(|pending| pending.borrow_mut().pop()) else {
            break;
        };
        if epoch != current_epoch() {
            // A reset already dropped (or will drop) this payload with its slab.
            continue;
        }
        let ptr = ARENA
            .try_with(|arena| arena.borrow_mut().begin_drop(kind, idx as usize))
            .ok()
            .flatten();
        let Some(ptr) = ptr else {
            continue;
        };
        // Safety: `begin_drop` marked the slot uninitialized, so neither a
        // reset `clear` nor another drain drops it again, and the slot is
        // not on the free list yet. Its chunk stays alive: a reset during
        // this drop parks the retired slabs in `GRAVEYARD` until the loop
        // ends. The arena is not borrowed here, so the payload's own
        // children re-enter `release` and only append to `PENDING`.
        unsafe { ptr.drop_payload() };
        if epoch == current_epoch() {
            let _ = ARENA.try_with(|arena| arena.borrow_mut().finish_drop(kind, idx as usize));
        }
    }
    drop(guard);
    let graves = GRAVEYARD
        .try_with(|graveyard| std::mem::take(&mut *graveyard.borrow_mut()))
        .unwrap_or_default();
    drop(graves);
}

/// Full slot generation of a live handle, for weak references.
pub(crate) fn generation(kind: SlotKind, handle: u32) -> Option<u32> {
    ARENA.with(|arena| arena.borrow().generation(kind, handle))
}

/// Identity bits (63 bits; bit 63 is left for callers to tag) for a live
/// handle. Unlike the slot address they stay unique across slot reuse:
/// slab kind (bits 61..63), low epoch bits (56..61), full slot generation
/// (24..56) and slot index (0..24).
pub(crate) fn identity_bits(kind: SlotKind, handle: u32) -> Option<u64> {
    let generation = generation(kind, handle)?;
    let kind = match kind {
        SlotKind::Object => 0_u64,
        SlotKind::Array => 1,
        SlotKind::Function => 2,
    };
    let epoch = (current_epoch() as u64) & 0x1F;
    Some(
        (kind << 61)
            | (epoch << 56)
            | ((generation as u64) << INDEX_BITS)
            | (handle & INDEX_MASK) as u64,
    )
}

/// [`identity_bits`] as a `usize` identity word tagged with the top bit, so
/// it never equals a host allocation address.
pub(crate) fn identity_word(kind: SlotKind, handle: u32) -> Option<usize> {
    let bits = identity_bits(kind, handle)? | (1 << 63);
    Some(if usize::BITS >= 64 {
        bits as usize
    } else {
        ((bits ^ (bits >> 32)) as usize) | (1 << (usize::BITS - 1))
    })
}

/// Strong count of a live slot (tests / diagnostics).
#[cfg(test)]
pub(crate) fn strong_count_of(kind: SlotKind, handle: u32) -> Option<u32> {
    ARENA.with(|arena| arena.borrow().strong_count(kind, handle))
}

/// Store a page-local object with one strong count (owned by the returned
/// handle). `Err` returns the payload when the slab handle space is full.
pub(crate) fn alloc_object(object: crate::JsObject) -> Result<u32, crate::JsObject> {
    ARENA.with(|arena| arena.borrow_mut().alloc_object(object))
}

/// Resolve a live object handle into a counted ref. `None` for a dead,
/// reused, or previous-page handle. Does not clone the payload.
pub(crate) fn get_object(handle: u32) -> Option<crate::value::JsObjectRef> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| arena.borrow_mut().get_object(handle))
}

/// Upgrade an interned weak object. Fails after the slot died (count zero,
/// generation bumped) or after [`reset`] (epoch).
pub(crate) fn upgrade_object(
    handle: u32,
    epoch: u32,
    generation: u32,
) -> Option<crate::value::JsObjectRef> {
    if handle == 0 || epoch != current_epoch() {
        return None;
    }
    ARENA.with(|arena| arena.borrow_mut().upgrade_object(handle, epoch, generation))
}

/// Live (strongly referenced) page-local objects.
pub fn live_objects() -> usize {
    ARENA.with(|arena| arena.borrow().objects.len())
}

/// Store a page-local array; see [`alloc_object`].
pub(crate) fn alloc_array(
    array: crate::value::ArrayStorage,
) -> Result<u32, crate::value::ArrayStorage> {
    ARENA.with(|arena| arena.borrow_mut().alloc_array(array))
}

/// Resolve a live array handle into a counted ref; see [`get_object`].
pub(crate) fn get_array(handle: u32) -> Option<crate::value::JsArrayRef> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| arena.borrow_mut().get_array(handle))
}

/// Upgrade an interned weak array; see [`upgrade_object`].
pub(crate) fn upgrade_array(
    handle: u32,
    epoch: u32,
    generation: u32,
) -> Option<crate::value::JsArrayRef> {
    if handle == 0 || epoch != current_epoch() {
        return None;
    }
    ARENA.with(|arena| arena.borrow_mut().upgrade_array(handle, epoch, generation))
}

/// Live (strongly referenced) page-local arrays.
pub fn live_arrays() -> usize {
    ARENA.with(|arena| arena.borrow().arrays.len())
}

/// Store a page-local JS function; see [`alloc_object`].
pub(crate) fn alloc_function(
    function: crate::value::FunctionData,
) -> Result<u32, crate::value::FunctionData> {
    ARENA.with(|arena| arena.borrow_mut().alloc_function(function))
}

/// Resolve a live function handle into a counted ref; see [`get_object`].
pub(crate) fn get_function(handle: u32) -> Option<crate::value::JsFunction> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| arena.borrow_mut().get_function(handle))
}

/// Upgrade an interned weak function; see [`upgrade_object`].
pub(crate) fn upgrade_function(
    handle: u32,
    epoch: u32,
    generation: u32,
) -> Option<crate::value::JsFunction> {
    if handle == 0 || epoch != current_epoch() {
        return None;
    }
    ARENA.with(|arena| {
        arena
            .borrow_mut()
            .upgrade_function(handle, epoch, generation)
    })
}

/// Live (strongly referenced) page-local functions.
pub fn live_functions() -> usize {
    ARENA.with(|arena| arena.borrow().functions.len())
}

/// One live slot as seen by [`crate::leak_detector`].
#[cfg(feature = "leak-detector")]
pub(crate) struct LiveSlot {
    pub(crate) kind: SlotKind,
    pub(crate) index: usize,
    pub(crate) generation: u32,
    pub(crate) strong: u32,
    pub(crate) payload: crate::leak_detector::Payload,
}

/// Snapshot every live slot's strong count and payload pointer. Read-only:
/// no count changes, nothing freed.
#[cfg(feature = "leak-detector")]
pub(crate) fn scan_live_slots() -> Vec<LiveSlot> {
    use crate::leak_detector::Payload;
    fn scan<T>(
        slab: &SizeClassSlab<T>,
        out: &mut Vec<LiveSlot>,
        wrap: impl Fn(*const T) -> Payload,
    ) {
        for (index, meta) in slab.meta.iter().enumerate() {
            if meta.strong.get() == 0 || !meta.init.get() {
                continue;
            }
            out.push(LiveSlot {
                kind: slab.kind,
                index,
                generation: meta.generation.get(),
                strong: meta.strong.get(),
                payload: wrap(slab.slot_ptr(index) as *const T),
            });
        }
    }
    ARENA.with(|arena| {
        let arena = arena.borrow();
        let mut out = Vec::new();
        scan(&arena.objects, &mut out, Payload::Object);
        scan(&arena.arrays, &mut out, Payload::Array);
        scan(&arena.functions, &mut out, Payload::Function);
        out
    })
}

/// Slot index of a handle (no liveness check).
#[cfg(feature = "leak-detector")]
pub(crate) fn handle_index(handle: u32) -> Option<usize> {
    decode_handle(handle).map(|(index, _)| index)
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
        let handle = alloc_object(crate::JsObject::new()).ok().expect("slot");
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
        let handle = alloc_array(crate::value::ArrayStorage::new(Vec::new()))
            .ok()
            .expect("slot");
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
        }))
        .ok()
        .expect("slot");
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
        assert_eq!(
            obj_slots, 13,
            "lazy-boxed rare JsObject fields densify RefCell slab (was 8 at 480B)"
        );
        let n = fn_slots.min(obj_slots).min(16);
        assert!(n >= 8);

        for _ in 0..n {
            let _ = alloc_function(crate::value::FunctionData::new(|_, _| {
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

    fn object_value(props: &[(&str, crate::Value)]) -> crate::Value {
        let mut map = std::collections::HashMap::new();
        for (key, value) in props {
            map.insert((*key).to_string(), value.clone());
        }
        crate::Value::object(map)
    }

    fn handle_of(value: &crate::Value) -> u32 {
        value.object_handle().expect("page-arena object")
    }

    #[test]
    fn slot_strong_count_tracks_clone_and_drop() {
        reset();
        let value = object_value(&[]);
        let handle = handle_of(&value);
        assert_eq!(strong_count_of(SlotKind::Object, handle), Some(1));
        let copy = value.clone();
        assert_eq!(strong_count_of(SlotKind::Object, handle), Some(2));
        let resolved = value.as_object().expect("object");
        assert_eq!(strong_count_of(SlotKind::Object, handle), Some(3));
        let resolved_copy = resolved.clone();
        assert_eq!(strong_count_of(SlotKind::Object, handle), Some(4));
        drop(resolved_copy);
        drop(resolved);
        drop(copy);
        assert_eq!(strong_count_of(SlotKind::Object, handle), Some(1));
        assert_eq!(live_objects(), 1);
        drop(value);
        assert_eq!(strong_count_of(SlotKind::Object, handle), None);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn freed_slot_is_reused_with_a_new_generation() {
        reset();
        let first = object_value(&[("tag", crate::Value::Number(1.0))]);
        let old_handle = handle_of(&first);
        let old_bits = first.object_identity_bits().expect("identity");
        let old_identity = first.as_object().expect("object").identity();
        let weak = first.as_object().expect("object").downgrade();
        drop(first);
        assert!(weak.upgrade().is_none(), "weak ref dies with the slot");
        assert!(get_object(old_handle).is_none(), "stale handle is rejected");

        let second = object_value(&[("tag", crate::Value::Number(2.0))]);
        let new_handle = handle_of(&second);
        assert_eq!(
            new_handle & INDEX_MASK,
            old_handle & INDEX_MASK,
            "free list reuses the slot"
        );
        assert_ne!(new_handle, old_handle, "generation byte differs");
        assert_ne!(second.object_identity_bits(), Some(old_bits));
        assert_ne!(
            second.as_object().expect("object").identity(),
            old_identity,
            "identity-keyed side tables never alias a reused slot"
        );
        assert!(get_object(old_handle).is_none());
        assert!(
            weak.upgrade().is_none(),
            "old weak ref never sees the new occupant"
        );
        assert_eq!(second.get_property("tag"), crate::Value::Number(2.0));
        assert_eq!(slab_bytes(), {
            let size = std::mem::size_of::<RefCell<crate::JsObject>>();
            SizeClassSlab::<RefCell<crate::JsObject>>::slots_per_chunk() * size
        });
        reset();
    }

    #[test]
    fn full_generation_guards_weak_refs_across_generation_byte_wrap() {
        reset();
        let first = object_value(&[]);
        let weak = first.as_object().expect("object").downgrade();
        let handle = handle_of(&first);
        drop(first);
        // 256 reuses bring the handle's generation byte back around.
        let mut reused = None;
        for _ in 0..256 {
            reused = Some(object_value(&[]));
            if reused.as_ref().map(handle_of) == Some(handle) {
                break;
            }
            reused = None;
        }
        let reused = reused.expect("same handle after the generation byte wraps");
        assert_eq!(handle_of(&reused), handle);
        assert!(weak.upgrade().is_none(), "full generation still differs");
        reset();
    }

    #[test]
    fn escaping_objects_are_freed_before_reset() {
        reset();
        let base = live_objects();
        {
            let items: Vec<crate::Value> = (0..1000)
                .map(|index| {
                    let inner = object_value(&[("i", crate::Value::Number(index as f64))]);
                    object_value(&[("inner", inner)])
                })
                .collect();
            let array = crate::Value::array(items);
            assert_eq!(live_objects(), base + 2000);
            assert_eq!(live_arrays(), 1);
            let holder = object_value(&[("list", array)]);
            assert_eq!(live_objects(), base + 2001);
            drop(holder);
        }
        assert_eq!(live_objects(), base);
        assert_eq!(live_arrays(), 0);
        reset();
    }

    #[test]
    fn overwriting_a_property_releases_the_old_value() {
        reset();
        let holder = object_value(&[]);
        holder.set_property("slot", object_value(&[]));
        assert_eq!(live_objects(), 2);
        holder.set_property("slot", crate::Value::Number(0.0));
        assert_eq!(live_objects(), 1);
        drop(holder);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn long_chain_is_released_iteratively() {
        reset();
        // Deep enough to overflow a 2 MiB test-thread stack with recursive drops.
        let mut head = object_value(&[]);
        for _ in 0..200_000 {
            head = object_value(&[("next", head)]);
        }
        assert_eq!(live_objects(), 200_001);
        drop(head);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn closures_and_their_captures_are_freed() {
        reset();
        let captured = object_value(&[("n", crate::Value::Number(41.0))]);
        let inside = captured.clone();
        let function = crate::Value::function(move |_, _| {
            crate::Value::Number(inside.get_property("n").as_number().unwrap_or(0.0) + 1.0)
        });
        drop(captured);
        assert_eq!(live_functions(), 1);
        // The capture plus the function's own `prototype` object.
        assert_eq!(live_objects(), 2);
        assert_eq!(
            function.call(crate::Value::Undefined, vec![]),
            crate::Value::Number(42.0)
        );
        drop(function);
        assert_eq!(live_functions(), 0);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn host_values_holding_arena_values_release_them() {
        reset();
        let host = crate::Value::Object(std::rc::Rc::new(RefCell::new(crate::JsObject::new())));
        host.set_property("child", object_value(&[]));
        assert_eq!(live_objects(), 1);
        drop(host);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn cycles_stay_alive_until_reset() {
        reset();
        let a = object_value(&[]);
        let b = object_value(&[("peer", a.clone())]);
        a.set_property("peer", b.clone());
        let selfish = object_value(&[]);
        selfish.set_property("me", selfish.clone());
        drop(a);
        drop(b);
        drop(selfish);
        // Strong refcounting cannot see cycles; they are the documented
        // leak that `reset` (or `unique_back_edges` weakening) reclaims.
        assert_eq!(live_objects(), 3);
        reset();
        assert_eq!(live_objects(), 0);
        assert_eq!(slab_bytes(), 0);
    }

    #[test]
    fn values_from_a_previous_page_are_inert() {
        reset();
        let old = object_value(&[]);
        let old_handle = handle_of(&old);
        let old_ref = old.as_object().expect("object");
        reset();
        let current = object_value(&[]);
        assert_eq!(
            handle_of(&current),
            old_handle,
            "same slot index and generation"
        );
        drop(old_ref);
        drop(old.clone());
        drop(old);
        assert_eq!(
            strong_count_of(SlotKind::Object, handle_of(&current)),
            Some(1)
        );
        assert_eq!(live_objects(), 1);
        drop(current);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn reset_during_a_payload_drop_is_deferred() {
        reset();
        struct ResetOnDrop;
        impl Drop for ResetOnDrop {
            fn drop(&mut self) {
                super::reset();
            }
        }
        let guard = std::rc::Rc::new(ResetOnDrop);
        let function = crate::Value::function(move |_, _| {
            let _keep = &guard;
            crate::Value::Undefined
        });
        let sibling = object_value(&[("f", function)]);
        let other = object_value(&[]);
        drop(sibling);
        // The reset ran inside the closure's drop; its slabs were parked and
        // dropped once the drain finished.
        assert_eq!(live_objects(), 0);
        assert_eq!(live_functions(), 0);
        drop(other);
        assert_eq!(live_objects(), 0);
        reset();
    }

    fn slot_of(value: &crate::Value) -> (SlotKind, u32) {
        match value {
            crate::Value::Imm(imm) => imm.arena_slot().expect("arena object"),
            _ => panic!("expected an arena handle"),
        }
    }

    #[test]
    fn fast_count_path_follows_meta_buffer_growth() {
        reset();
        let value = object_value(&[]);
        let (kind, handle) = slot_of(&value);
        let first = value.clone();
        // Grow (and so reallocate) the meta buffer many times while counts
        // are held; every later clone/drop must use the moved buffer.
        let many: Vec<crate::Value> = (0..20_000).map(|_| object_value(&[])).collect();
        let second = value.clone();
        assert_eq!(strong_count_of(kind, handle), Some(3));
        drop(first);
        drop(second);
        assert_eq!(strong_count_of(kind, handle), Some(1));
        let other = many[12_345].clone();
        let (other_kind, other_handle) = slot_of(&other);
        assert_eq!(strong_count_of(other_kind, other_handle), Some(2));
        drop(many);
        assert_eq!(strong_count_of(other_kind, other_handle), Some(1));
        assert_eq!(live_objects(), 2);
        drop(other);
        drop(value);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn fast_count_path_ignores_previous_page_handles() {
        reset();
        let stale = object_value(&[]);
        assert_eq!(slot_of(&stale).1, 1);
        let stale_copy = stale.clone();
        reset();
        // Same slot index and generation byte on the new page.
        let fresh = object_value(&[]);
        let (kind, handle) = slot_of(&fresh);
        // Both pages handed out slot 0 with generation 0 first.
        assert_eq!(handle, 1);
        let again = stale.clone();
        drop(again);
        drop(stale_copy);
        drop(stale);
        assert_eq!(strong_count_of(kind, handle), Some(1));
        assert_eq!(live_objects(), 1);
        drop(fresh);
        assert_eq!(live_objects(), 0);
        reset();
    }

    #[test]
    fn thread_teardown_with_counted_children_does_not_touch_freed_meta() {
        std::thread::spawn(|| {
            let parent = object_value(&[]);
            let child = object_value(&[]);
            parent.set_property("child", child.clone());
            // A self-cycle keeps `parent` alive until the arena itself is
            // destroyed with the thread; its payload then releases `child`
            // while the slab is being torn down.
            parent.set_property("self", parent.clone());
            drop(child);
            drop(parent);
            assert_eq!(live_objects(), 2);
        })
        .join()
        .expect("teardown");
    }
}

#[cfg(feature = "rc-stats")]
thread_local! {
    static RC_OPS: [Cell<u64>; 2] = const { [Cell::new(0), Cell::new(0)] };
}

/// `(retains, releases)` of arena slots on this thread since start (or the
/// last [`reset_rc_op_counts`]). Only with the `rc-stats` feature.
#[cfg(feature = "rc-stats")]
pub fn rc_op_counts() -> (u64, u64) {
    RC_OPS.with(|ops| (ops[0].get(), ops[1].get()))
}

#[cfg(feature = "rc-stats")]
pub fn reset_rc_op_counts() {
    RC_OPS.with(|ops| {
        ops[0].set(0);
        ops[1].set(0);
    });
}
