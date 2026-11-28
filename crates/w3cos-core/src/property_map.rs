//! Shape-based property storage for [`crate::JsObject`]s and function objects.
//!
//! An ordinary object stores its own string-keyed properties as a pointer to
//! a shared [`Shape`] (hidden class) plus a flat `Vec<Value>` of slots. A
//! shape is a node in a per-thread transition tree keyed by property name:
//! objects that receive the same keys in the same order share one shape, so
//! the key list (and a key -> slot index for wide shapes) is stored once.
//! Each shape has a process-unique [`ShapeId`], which is what AOT inline
//! caches compare.
//!
//! Objects fall back to *dictionary mode* (object-local `HashMap` + ordered
//! entry list, the pre-shape representation) when a property is deleted,
//! when they grow past [`MAX_SHAPE_KEYS`], or when the thread's shape budget
//! ([`MAX_SHAPES`]) is exhausted. Dictionary objects never hit an IC.
//!
//! Shapes are an append-only cache: they are never freed (bounded by
//! [`MAX_SHAPES`]), hold no `Value`s, and store their keys as heap strings so
//! they survive `page_arena::reset`. They therefore never keep a JS value
//! alive and do not affect refcount / weak semantics.
//!
//! Key enumeration follows ECMAScript `OrdinaryOwnPropertyKeys` for string
//! keys: canonical array-index keys first in ascending numeric order, then
//! the remaining keys in insertion order.

use std::cell::{Cell, OnceCell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::mem::MaybeUninit;

use crate::fast_hash::FastMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::js_string::JsString;
use crate::value::Value;

/// Own keys before an object leaves the shape tree for dictionary mode.
pub const MAX_SHAPE_KEYS: usize = 64;
/// Shapes one thread may create before new transitions go to dictionary mode.
pub const MAX_SHAPES: usize = 1 << 15;
/// Prefix of internal property names (`__w3cos_getter_k`, `__w3cos_setter_k`).
pub(crate) const MARKER_PREFIX: &str = "__w3cos_";
/// Shapes with at most this many keys are searched linearly.
const LINEAR_SCAN: usize = 8;
/// Linear lookups a wide shape serves before it builds its key index.
const INDEX_AFTER_SCANS: u8 = 8;
/// Transition lists longer than this get a hash index.
const LINEAR_TRANSITIONS: usize = 8;

/// Process-unique shape identity. `0` never names a shape (IC "empty").
/// [`ROOT_SHAPE_ID`] is the empty shape on every thread (the key list is
/// empty, so sharing the id across threads is sound).
pub type ShapeId = u32;
pub const ROOT_SHAPE_ID: ShapeId = 1;
static NEXT_SHAPE_ID: AtomicU32 = AtomicU32::new(ROOT_SHAPE_ID + 1);

/// True when `key` is a canonical array index (`"0"`, `"17"`, not `"01"`).
pub(crate) fn is_index_key(key: &str) -> bool {
    index_key_value(key).is_some()
}

fn index_key_value(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 {
        return None;
    }
    if bytes[0] == b'0' {
        return (bytes.len() == 1).then_some(0);
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    // Array indices are < 2^32 - 1.
    (value < u32::MAX as u64).then_some(value as u32)
}

/// Append-only key storage shared along a transition chain.
///
/// Filling a wide object key by key (a class prototype, a constructor table)
/// walks one chain root -> k1 -> k1,k2 -> ... and each of those shapes used
/// to own a full copy of its key list, so the chain's key memory was
/// quadratic in its width. A child that extends the chain's last key shares
/// its parent's buffer and only records a longer length; a branch (a second
/// child of a non-tip shape) or a full buffer starts a new buffer with a copy
/// of the prefix. Buffers grow by doubling, so a chain of `n` keys stores at
/// most ~`2n` keys.
///
/// Slots below `len` are written once and never changed or moved, so a
/// shape's `&[JsString]` view of `slots[..its len]` stays valid while later
/// shapes append past it.
struct KeyBuf {
    slots: Box<[UnsafeCell<MaybeUninit<JsString>>]>,
    len: Cell<usize>,
}

impl KeyBuf {
    fn with_prefix(prefix: &[JsString], capacity: usize) -> Rc<Self> {
        debug_assert!(capacity >= prefix.len());
        let slots: Box<[UnsafeCell<MaybeUninit<JsString>>]> = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();
        let buf = Self {
            slots,
            len: Cell::new(0),
        };
        for key in prefix {
            buf.try_push(key.clone())
                .unwrap_or_else(|_| unreachable!("capacity covers the prefix"));
        }
        Rc::new(buf)
    }

    /// Writes the next slot. `Err` when the buffer is full.
    fn try_push(&self, key: JsString) -> Result<(), JsString> {
        let len = self.len.get();
        let Some(slot) = self.slots.get(len) else {
            return Err(key);
        };
        // Safety: slot `len` has never been written and no view covers it
        // (views only cover `..len`).
        unsafe { (*slot.get()).write(key) };
        self.len.set(len + 1);
        Ok(())
    }

    #[inline]
    fn prefix(&self, len: usize) -> &[JsString] {
        assert!(len <= self.len.get(), "key view past the written slots");
        // Safety: `UnsafeCell<MaybeUninit<T>>` has `T`'s layout, slots below
        // `self.len` are initialised and are never written again.
        unsafe { std::slice::from_raw_parts(self.slots.as_ptr().cast::<JsString>(), len) }
    }
}

impl Drop for KeyBuf {
    fn drop(&mut self) {
        for slot in &mut self.slots[..self.len.get()] {
            // Safety: every slot below `len` was initialised exactly once.
            unsafe { slot.get_mut().assume_init_drop() };
        }
    }
}

enum Transitions {
    Few(Vec<(JsString, Rc<Shape>)>),
    Many(FastMap<JsString, Rc<Shape>>),
}

/// A hidden class: an ordered own-key list shared by many objects.
pub(crate) struct Shape {
    id: ShapeId,
    /// Insertion-ordered own keys are `key_buf[..key_len]`; slot `i` of an
    /// object holds key `i`. The buffer may be shared with descendants.
    key_buf: Rc<KeyBuf>,
    key_len: usize,
    /// Key -> slot for shapes wider than [`LINEAR_SCAN`], built on first use.
    index: OnceCell<FastMap<JsString, u32>>,
    /// Linear lookups served while `index` is still unbuilt.
    linear_scans: Cell<u8>,
    /// Any key is a canonical array index (enumeration must reorder).
    has_index_key: bool,
    /// Any key is an internal `__w3cos_` marker (accessor halves etc.).
    has_marker_key: bool,
    transitions: RefCell<Transitions>,
}

impl Shape {
    fn root() -> Self {
        Self {
            id: ROOT_SHAPE_ID,
            key_buf: KeyBuf::with_prefix(&[], 0),
            key_len: 0,
            index: OnceCell::new(),
            linear_scans: Cell::new(0),
            has_index_key: false,
            has_marker_key: false,
            transitions: RefCell::new(Transitions::Few(Vec::new())),
        }
    }

    /// Insertion-ordered own keys.
    #[inline]
    pub(crate) fn keys(&self) -> &[JsString] {
        self.key_buf.prefix(self.key_len)
    }

    #[inline]
    pub(crate) fn lookup(&self, key: &str) -> Option<usize> {
        if self.key_len <= LINEAR_SCAN {
            return self.linear_lookup(key);
        }
        if let Some(index) = self.index.get() {
            return index.get(key).map(|&i| i as usize);
        }
        // Most shapes past LINEAR_SCAN are transient prefixes of one wide
        // object being filled key by key (a class prototype, a constructor
        // table): looked up a handful of times, then never again. An index
        // for each of them made the memory quadratic in the final width.
        // A key this shape transitions on is not one of its own keys, and
        // only a shape that keeps being searched pays for an index.
        if self.transitions_on(key) {
            return None;
        }
        let scans = self.linear_scans.get();
        if scans < INDEX_AFTER_SCANS {
            self.linear_scans.set(scans + 1);
            return self.linear_lookup(key);
        }
        self.index
            .get_or_init(|| {
                self.keys()
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (k.clone(), i as u32))
                    .collect()
            })
            .get(key)
            .map(|&i| i as usize)
    }

    #[inline]
    fn linear_lookup(&self, key: &str) -> Option<usize> {
        self.keys().iter().position(|k| k.as_str() == key)
    }

    /// `key` names an existing child transition (so it is not an own key).
    fn transitions_on(&self, key: &str) -> bool {
        let Ok(transitions) = self.transitions.try_borrow() else {
            return false;
        };
        match &*transitions {
            Transitions::Few(list) => list.iter().any(|(k, _)| k.as_str() == key),
            Transitions::Many(map) => map.contains_key(key),
        }
    }

    /// Child shape with `key` appended, creating it if the budget allows.
    fn transition(self: &Rc<Self>, key: &str) -> Option<Rc<Shape>> {
        {
            let transitions = self.transitions.borrow();
            let found = match &*transitions {
                Transitions::Few(list) => list
                    .iter()
                    .find(|(k, _)| k.as_str() == key)
                    .map(|(_, shape)| Rc::clone(shape)),
                Transitions::Many(map) => map.get(key).cloned(),
            };
            if found.is_some() {
                return found;
            }
        }
        if self.key_len >= MAX_SHAPE_KEYS {
            return None;
        }
        let count = SHAPE_COUNT.with(|count| {
            let next = count.get() + 1;
            count.set(next);
            next
        });
        if count > MAX_SHAPES {
            return None;
        }
        let key = JsString::heap(key);
        // Extend this chain's buffer when this shape is its tip; otherwise
        // (a branch, or a full buffer) copy the prefix into a new one.
        let shares =
            self.key_buf.len.get() == self.key_len && self.key_buf.try_push(key.clone()).is_ok();
        let key_buf = if shares {
            Rc::clone(&self.key_buf)
        } else {
            let capacity = (self.key_len + 1)
                .next_power_of_two()
                .clamp(4, MAX_SHAPE_KEYS);
            let buf = KeyBuf::with_prefix(self.keys(), capacity);
            buf.try_push(key.clone())
                .unwrap_or_else(|_| unreachable!("a new buffer has room for the key"));
            buf
        };
        let child = Rc::new(Shape {
            id: NEXT_SHAPE_ID.fetch_add(1, Ordering::Relaxed),
            has_index_key: self.has_index_key || is_index_key(key.as_str()),
            has_marker_key: self.has_marker_key || key.as_str().starts_with(MARKER_PREFIX),
            key_buf,
            key_len: self.key_len + 1,
            index: OnceCell::new(),
            linear_scans: Cell::new(0),
            transitions: RefCell::new(Transitions::Few(Vec::new())),
        });
        let mut transitions = self.transitions.borrow_mut();
        match &mut *transitions {
            Transitions::Few(list) => {
                list.push((key, Rc::clone(&child)));
                if list.len() > LINEAR_TRANSITIONS {
                    let map = std::mem::take(list).into_iter().collect();
                    *transitions = Transitions::Many(map);
                }
            }
            Transitions::Many(map) => {
                map.insert(key, Rc::clone(&child));
            }
        }
        Some(child)
    }
}

thread_local! {
    static ROOT: Rc<Shape> = Rc::new(Shape::root());
    static SHAPE_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Shapes created on this thread (tests / diagnostics).
pub fn shape_count() -> usize {
    SHAPE_COUNT.with(Cell::get).min(MAX_SHAPES)
}

fn root_shape() -> Rc<Shape> {
    ROOT.with(Rc::clone)
}

#[derive(Clone)]
pub(crate) struct PropertyMap {
    inner: Option<PropertyStore>,
}

/// Header of a shape-backed store. The slot values follow it in the same
/// allocation, so a shaped object costs one malloc of 16 + 16 * capacity
/// bytes (a separate `Box` holding a shape pointer plus a slot `Vec` took
/// two: 32 bytes for the box and the slot buffer on top).
#[repr(C)]
struct ShapedHeader {
    shape: Rc<Shape>,
    len: u32,
    cap: u32,
}

const _: () = assert!(std::mem::size_of::<ShapedHeader>() % std::mem::align_of::<Value>() == 0);
const _: () = assert!(std::mem::align_of::<ShapedHeader>() >= 2);
const _: () = assert!(std::mem::align_of::<DictStore>() >= 2);

/// Owning pointer to a [`ShapedHeader`] and its trailing slots.
#[repr(transparent)]
struct ShapedBuf(std::ptr::NonNull<ShapedHeader>);

impl ShapedBuf {
    fn layout(cap: usize) -> std::alloc::Layout {
        let slots = std::alloc::Layout::array::<Value>(cap).expect("slot layout");
        std::alloc::Layout::new::<ShapedHeader>()
            .extend(slots)
            .expect("shaped layout")
            .0
            .pad_to_align()
    }

    fn with_capacity(shape: Rc<Shape>, cap: usize) -> Self {
        assert!(cap <= u32::MAX as usize, "shaped slot capacity");
        let layout = Self::layout(cap);
        // Safety: the layout is never zero-sized (it holds the header).
        let raw = unsafe { std::alloc::alloc(layout) }.cast::<ShapedHeader>();
        let Some(ptr) = std::ptr::NonNull::new(raw) else {
            std::alloc::handle_alloc_error(layout);
        };
        // Safety: freshly allocated with room for the header.
        unsafe {
            ptr.as_ptr().write(ShapedHeader {
                shape,
                len: 0,
                cap: cap as u32,
            })
        };
        Self(ptr)
    }

    fn from_vec(shape: Rc<Shape>, slots: Vec<Value>) -> Self {
        let mut buf = Self::with_capacity(shape, slots.len());
        for value in slots {
            buf.push(value);
        }
        buf
    }

    #[inline]
    fn header(&self) -> &ShapedHeader {
        // Safety: the pointer is live and owned for as long as `self`.
        unsafe { self.0.as_ref() }
    }

    #[inline]
    fn values(&self) -> *mut Value {
        // Safety: the slots start right after the header (no padding; see
        // the const asserts) inside the same allocation.
        unsafe { self.0.as_ptr().add(1).cast::<Value>() }
    }

    #[inline]
    fn shape(&self) -> &Rc<Shape> {
        &self.header().shape
    }

    #[inline]
    fn set_shape(&mut self, shape: Rc<Shape>) {
        // Safety: unique access through `&mut self`.
        unsafe { self.0.as_mut().shape = shape };
    }

    #[inline]
    fn len(&self) -> usize {
        self.header().len as usize
    }

    #[inline]
    fn slots(&self) -> &[Value] {
        // Safety: the first `len` slots are initialised.
        unsafe { std::slice::from_raw_parts(self.values(), self.len()) }
    }

    #[inline]
    fn slots_mut(&mut self) -> &mut [Value] {
        // Safety: as `slots`, with unique access through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.values(), self.len()) }
    }

    fn push(&mut self, value: Value) {
        let (len, cap) = (self.header().len as usize, self.header().cap as usize);
        if len == cap {
            // Double from 1 (1, 2, 4, 8, ...): small objects stay exact.
            let new_cap = cap.max(1).saturating_mul(2).max(len + 1);
            assert!(new_cap <= u32::MAX as usize, "shaped slot capacity");
            let new_layout = Self::layout(new_cap);
            // Safety: `self.0` was allocated with `layout(cap)`; the header
            // and the initialised slots move with the bytes.
            let raw = unsafe {
                std::alloc::realloc(self.0.as_ptr().cast(), Self::layout(cap), new_layout.size())
            }
            .cast::<ShapedHeader>();
            let Some(ptr) = std::ptr::NonNull::new(raw) else {
                std::alloc::handle_alloc_error(new_layout);
            };
            self.0 = ptr;
            // Safety: unique access.
            unsafe { self.0.as_mut().cap = new_cap as u32 };
        }
        // Safety: `len < cap`, so slot `len` is in bounds and uninitialised.
        unsafe {
            self.values().add(len).write(value);
            self.0.as_mut().len = (len + 1) as u32;
        }
    }

    /// The shape and the slot values; frees the buffer.
    fn into_parts(self) -> (Rc<Shape>, Vec<Value>) {
        let this = std::mem::ManuallyDrop::new(self);
        let len = this.len();
        let cap = this.header().cap as usize;
        let mut slots = Vec::with_capacity(len + 1);
        // Safety: the first `len` slots are initialised and are moved out
        // exactly once; the header's shape is read out once; then the
        // buffer is freed without dropping anything again.
        unsafe {
            std::ptr::copy_nonoverlapping(this.values(), slots.as_mut_ptr(), len);
            slots.set_len(len);
            let shape = std::ptr::read(&this.header().shape);
            std::alloc::dealloc(this.0.as_ptr().cast(), Self::layout(cap));
            (shape, slots)
        }
    }

    fn heap_bytes(&self) -> usize {
        Self::layout(self.header().cap as usize).size()
    }
}

impl Drop for ShapedBuf {
    fn drop(&mut self) {
        let cap = self.header().cap as usize;
        // Safety: drop the initialised slots and the header once, then free.
        unsafe {
            std::ptr::drop_in_place(self.slots_mut() as *mut [Value]);
            std::ptr::drop_in_place(self.0.as_ptr());
            std::alloc::dealloc(self.0.as_ptr().cast(), Self::layout(cap));
        }
    }
}

impl Clone for ShapedBuf {
    fn clone(&self) -> Self {
        let mut out = Self::with_capacity(Rc::clone(self.shape()), self.len());
        for value in self.slots() {
            out.push(value.clone());
        }
        out
    }
}

/// One word: a [`ShapedBuf`] pointer, or a `Box<DictStore>` pointer with
/// the low bit set. Both pointees are at least 2-aligned.
#[repr(transparent)]
struct PropertyStore(std::ptr::NonNull<u8>);

const DICT_TAG: usize = 1;

enum StoreRef<'a> {
    Shaped {
        shape: &'a Rc<Shape>,
        slots: &'a [Value],
    },
    Dict(&'a DictStore),
}

enum StoreMut<'a> {
    Shaped(&'a mut ShapedBuf),
    Dict(&'a mut DictStore),
}

impl PropertyStore {
    fn shaped(buf: ShapedBuf) -> Self {
        let buf = std::mem::ManuallyDrop::new(buf);
        Self(buf.0.cast())
    }

    fn dict(dict: Box<DictStore>) -> Self {
        let raw = Box::into_raw(dict).cast::<u8>();
        // Safety: a box pointer is non-null; setting the low bit of a
        // 2-aligned address keeps it non-null.
        Self(unsafe { std::ptr::NonNull::new_unchecked(raw.map_addr(|a| a | DICT_TAG)) })
    }

    #[inline]
    fn is_dict(&self) -> bool {
        self.0.as_ptr().addr() & DICT_TAG != 0
    }

    #[inline]
    fn dict_ptr(&self) -> *mut DictStore {
        self.0.as_ptr().map_addr(|a| a & !DICT_TAG).cast()
    }

    #[inline]
    fn view(&self) -> StoreRef<'_> {
        if self.is_dict() {
            // Safety: tagged pointers come from `Box::into_raw` and are owned.
            StoreRef::Dict(unsafe { &*self.dict_ptr() })
        } else {
            // Safety: untagged, the word is exactly a `ShapedBuf`.
            let buf = unsafe { &*(self as *const Self).cast::<ShapedBuf>() };
            StoreRef::Shaped {
                shape: buf.shape(),
                slots: buf.slots(),
            }
        }
    }

    #[inline]
    fn view_mut(&mut self) -> StoreMut<'_> {
        if self.is_dict() {
            // Safety: as `view`, with unique access.
            StoreMut::Dict(unsafe { &mut *self.dict_ptr() })
        } else {
            // Safety: untagged, the word is exactly a `ShapedBuf` (same
            // `repr(transparent)` pointer); a realloc through it updates us.
            StoreMut::Shaped(unsafe { &mut *(self as *mut Self).cast::<ShapedBuf>() })
        }
    }

    /// Take a shaped buffer out (`Err(self)` for a dictionary).
    fn into_shaped(self) -> Result<ShapedBuf, Self> {
        if self.is_dict() {
            return Err(self);
        }
        let this = std::mem::ManuallyDrop::new(self);
        Ok(ShapedBuf(this.0.cast()))
    }

    fn make_dictionary(&mut self) {
        if self.is_dict() {
            return;
        }
        // Safety: we own the shaped buffer; `self` is overwritten below
        // without dropping the moved-out word.
        let buf = ShapedBuf(self.0.cast());
        let (shape, slots) = buf.into_parts();
        let mut entries = Vec::with_capacity(slots.len() + 1);
        for (key, value) in shape.keys().iter().zip(slots) {
            entries.push((key.clone(), value));
        }
        let dict = Self::dict(Box::new(DictStore::from_entries(
            entries,
            shape.has_index_key,
        )));
        unsafe { std::ptr::write(self, dict) };
    }

    fn heap_bytes(&self) -> usize {
        match self.view() {
            StoreRef::Dict(dict) => dict.heap_bytes(),
            StoreRef::Shaped { .. } => {
                // Safety: untagged, see `view`.
                unsafe { &*(self as *const Self).cast::<ShapedBuf>() }.heap_bytes()
            }
        }
    }
}

impl Drop for PropertyStore {
    fn drop(&mut self) {
        if self.is_dict() {
            // Safety: tagged pointers come from `Box::into_raw`.
            drop(unsafe { Box::from_raw(self.dict_ptr()) });
        } else {
            drop(ShapedBuf(self.0.cast()));
        }
    }
}

impl Clone for PropertyStore {
    fn clone(&self) -> Self {
        match self.view() {
            StoreRef::Dict(dict) => Self::dict(Box::new(dict.clone())),
            StoreRef::Shaped { .. } => {
                // Safety: untagged, see `view`.
                let buf = unsafe { &*(self as *const Self).cast::<ShapedBuf>() };
                Self::shaped(buf.clone())
            }
        }
    }
}

/// Dictionary-mode storage: entries in insertion order plus, once there
/// are more than [`DICT_LINEAR`] of them, a hash index of entry positions.
///
/// This used to be a `FastMap<JsString, Value>` plus a separate `order`
/// list, which stored every key twice and sized the 32-byte buckets by
/// hashbrown's load factor. The index only holds `u32` positions, so a
/// large dictionary costs about 32 bytes per entry plus about 5 per index
/// bucket.
#[derive(Clone)]
struct DictStore {
    entries: Vec<(JsString, Value)>,
    /// Positions into `entries`, hashed by key. Empty while
    /// `entries.len() <= DICT_LINEAR` (lookups scan instead).
    index: hashbrown::HashTable<u32>,
    has_index_key: bool,
}

/// Dictionaries with at most this many entries have no hash index.
const DICT_LINEAR: usize = 8;

#[inline]
fn key_hash(key: &str) -> u64 {
    use std::hash::BuildHasher;
    rustc_hash::FxBuildHasher.hash_one(key)
}

impl DictStore {
    fn from_entries(entries: Vec<(JsString, Value)>, has_index_key: bool) -> Self {
        let mut dict = Self {
            entries,
            index: hashbrown::HashTable::new(),
            has_index_key,
        };
        if dict.entries.len() > DICT_LINEAR {
            dict.rebuild_index();
        }
        dict
    }

    #[inline]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn rebuild_index(&mut self) {
        let Self { entries, index, .. } = self;
        index.clear();
        index.reserve(entries.len(), |&i| key_hash(entries[i as usize].0.as_str()));
        for (i, (key, _)) in entries.iter().enumerate() {
            index.insert_unique(key_hash(key.as_str()), i as u32, |&j| {
                key_hash(entries[j as usize].0.as_str())
            });
        }
    }

    #[inline]
    fn position(&self, key: &str) -> Option<usize> {
        if self.index.is_empty() {
            return self.entries.iter().position(|(k, _)| k.as_str() == key);
        }
        self.index
            .find(key_hash(key), |&i| {
                self.entries[i as usize].0.as_str() == key
            })
            .map(|&i| i as usize)
    }

    #[inline]
    fn get(&self, key: &str) -> Option<&Value> {
        let i = self.position(key)?;
        Some(&self.entries[i].1)
    }

    #[inline]
    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let i = self.position(key)?;
        Some(&mut self.entries[i].1)
    }

    /// Append a key known to be absent.
    fn push(&mut self, key: JsString, value: Value) {
        let i = self.entries.len();
        let hash = key_hash(key.as_str());
        self.entries.push((key, value));
        if !self.index.is_empty() {
            let Self { entries, index, .. } = self;
            index.insert_unique(hash, i as u32, |&j| {
                key_hash(entries[j as usize].0.as_str())
            });
        } else if self.entries.len() > DICT_LINEAR {
            self.rebuild_index();
        }
    }

    fn remove(&mut self, key: &str) -> Option<Value> {
        let pos = self.position(key)?;
        let (_, value) = self.entries.remove(pos);
        if !self.index.is_empty() {
            if self.entries.len() <= DICT_LINEAR {
                self.index = hashbrown::HashTable::new();
            } else {
                let pos = pos as u32;
                self.index.retain(|i| {
                    if *i == pos {
                        return false;
                    }
                    if *i > pos {
                        *i -= 1;
                    }
                    true
                });
            }
        }
        Some(value)
    }

    fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.entries
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(JsString, Value)>()),
            )
            .saturating_add(
                self.index
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>() + 1),
            )
    }
}

impl PropertyMap {
    #[inline]
    pub(crate) fn new() -> Self {
        Self { inner: None }
    }

    pub(crate) fn from_interned_hashmap(mut map: HashMap<JsString, Value>) -> Self {
        let mut out = Self::new();
        if map.is_empty() {
            return out;
        }
        // An unordered HashMap has no insertion order: use sorted key order
        // (deterministic, and shared by every literal with the same keys).
        let mut order: Vec<JsString> = map.keys().cloned().collect();
        order.sort();
        let mut slots = Vec::with_capacity(order.len());
        let mut shape = Some(root_shape());
        for key in &order {
            shape = shape.and_then(|shape| shape.transition(key.as_str()));
            if shape.is_none() {
                break;
            }
            slots.push(map.remove(key).expect("key from order"));
        }
        if let Some(shape) = shape {
            out.inner = Some(PropertyStore::shaped(ShapedBuf::from_vec(shape, slots)));
            return out;
        }
        // Too wide or out of shape budget: dictionary from the start.
        // `map` still holds the keys the shape walk did not consume.
        let has_index_key = order.iter().any(|key| is_index_key(key.as_str()));
        let consumed = slots.len();
        let mut entries = Vec::with_capacity(order.len());
        let mut order = order.into_iter();
        for value in slots {
            entries.push((order.next().expect("one key per slot"), value));
        }
        debug_assert_eq!(entries.len(), consumed);
        for key in order {
            let value = map.remove(&key).expect("key from order");
            entries.push((key, value));
        }
        out.inner = Some(PropertyStore::dict(Box::new(DictStore::from_entries(
            entries,
            has_index_key,
        ))));
        out
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self.inner.as_ref().map(PropertyStore::view) {
            None => 0,
            Some(StoreRef::Shaped { slots, .. }) => slots.len(),
            Some(StoreRef::Dict(dict)) => dict.len(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_none()
    }

    /// True when the map is empty or shape-backed (no per-object hash table).
    #[inline]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_compact(&self) -> bool {
        !matches!(
            self.inner.as_ref().map(PropertyStore::view),
            Some(StoreRef::Dict(_))
        )
    }

    /// True once the map has left the shape tree.
    #[inline]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_dictionary(&self) -> bool {
        !self.is_compact()
    }

    /// Shape id for IC checks: [`ROOT_SHAPE_ID`] when empty, `None` in
    /// dictionary mode.
    #[inline]
    pub(crate) fn shape_id(&self) -> Option<ShapeId> {
        match self.inner.as_ref().map(PropertyStore::view) {
            None => Some(ROOT_SHAPE_ID),
            Some(StoreRef::Shaped { shape, .. }) => Some(shape.id),
            Some(StoreRef::Dict(_)) => None,
        }
    }

    /// `(shape id, slot)` of an own key in a shape-backed map.
    #[inline]
    pub(crate) fn shaped_slot(&self, key: &str) -> Option<(ShapeId, usize)> {
        match self.inner.as_ref().map(PropertyStore::view) {
            Some(StoreRef::Shaped { shape, .. }) => Some((shape.id, shape.lookup(key)?)),
            _ => None,
        }
    }

    /// Slot value when the map currently has shape `id` (IC hit path).
    #[inline]
    pub(crate) fn slot_if_shape(&self, id: ShapeId, slot: usize) -> Option<&Value> {
        match self.inner.as_ref().map(PropertyStore::view) {
            Some(StoreRef::Shaped { shape, slots }) if shape.id == id => slots.get(slot),
            _ => None,
        }
    }

    /// Mutable slot when the map currently has shape `id` (IC hit path).
    #[inline]
    pub(crate) fn slot_mut_if_shape(&mut self, id: ShapeId, slot: usize) -> Option<&mut Value> {
        match self.inner.as_mut().map(PropertyStore::view_mut) {
            Some(StoreMut::Shaped(buf)) if buf.shape().id == id => buf.slots_mut().get_mut(slot),
            _ => None,
        }
    }

    /// Heap bytes owned by this object beyond the `Option<Box>` word. The
    /// shared shape is not charged to any one object.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.inner.as_ref().map_or(0, PropertyStore::heap_bytes)
    }

    #[inline]
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self.inner.as_ref().map(PropertyStore::view) {
            None => None,
            Some(StoreRef::Shaped { shape, slots }) => shape.lookup(key).and_then(|i| slots.get(i)),
            Some(StoreRef::Dict(dict)) => dict.get(key),
        }
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self.inner.as_mut().map(PropertyStore::view_mut) {
            None => None,
            Some(StoreMut::Shaped(buf)) => {
                let i = buf.shape().lookup(key)?;
                buf.slots_mut().get_mut(i)
            }
            Some(StoreMut::Dict(dict)) => dict.get_mut(key),
        }
    }

    #[inline]
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub(crate) fn contains_key_or_prefixed(
        &self,
        key: &str,
        first_prefix: &str,
        second_prefix: &str,
    ) -> bool {
        if self.contains_key(key) {
            return true;
        }
        // Fall back to a scan when accessor prefixes may be present.
        self.keys().any(|candidate| {
            let candidate = candidate.as_str();
            candidate.strip_prefix(first_prefix) == Some(key)
                || candidate.strip_prefix(second_prefix) == Some(key)
        })
    }

    pub(crate) fn insert(&mut self, key: JsString, value: Value) -> Option<Value> {
        if let Some(slot) = self.get_mut(key.as_str()) {
            return Some(std::mem::replace(slot, value));
        }
        self.add_new(key.as_str(), Some(key.clone()), value);
        None
    }

    /// Insert by `&str`; only allocates a key string when the key is new.
    pub(crate) fn insert_str(&mut self, key: &str, value: Value) -> Option<Value> {
        if let Some(slot) = self.get_mut(key) {
            return Some(std::mem::replace(slot, value));
        }
        self.add_new(key, None, value);
        None
    }

    /// Append a key known to be absent.
    fn add_new(&mut self, key: &str, interned: Option<JsString>, value: Value) {
        let store = match self.inner.as_mut() {
            Some(store) => store,
            None => {
                let root = root_shape();
                if let Some(shape) = root.transition(key) {
                    // Exact for one key: many objects (e.g. function
                    // prototypes holding only `constructor`) never get a
                    // second.
                    let mut buf = ShapedBuf::with_capacity(shape, 1);
                    buf.push(value);
                    self.inner = Some(PropertyStore::shaped(buf));
                    return;
                }
                self.inner
                    .insert(PropertyStore::dict(Box::new(DictStore::from_entries(
                        Vec::new(),
                        false,
                    ))))
            }
        };
        if let StoreMut::Shaped(buf) = store.view_mut() {
            if let Some(next) = buf.shape().transition(key) {
                buf.set_shape(next);
                buf.push(value);
                return;
            }
            store.make_dictionary();
        }
        let StoreMut::Dict(dict) = store.view_mut() else {
            unreachable!("dictionary after make_dictionary");
        };
        let key = interned.unwrap_or_else(|| JsString::intern(key));
        dict.has_index_key |= is_index_key(key.as_str());
        dict.push(key, value);
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<Value> {
        let only_key = match self.inner.as_ref()?.view() {
            StoreRef::Shaped { shape, slots } => {
                shape.lookup(key)?;
                slots.len() == 1
            }
            StoreRef::Dict(_) => false,
        };
        if only_key {
            let buf = self.inner.take()?.into_shaped().ok()?;
            let (_, mut slots) = buf.into_parts();
            return slots.pop();
        }
        let store = self.inner.as_mut()?;
        // Deletes leave the shape tree (dictionary mode), so a stale IC can
        // never resolve a slot index against the shifted slot layout.
        store.make_dictionary();
        let StoreMut::Dict(dict) = store.view_mut() else {
            unreachable!("dictionary after make_dictionary");
        };
        let removed = dict.remove(key)?;
        if dict.len() == 0 {
            self.inner = None;
        }
        Some(removed)
    }

    pub(crate) fn keys(&self) -> PropertyMapKeys<'_> {
        let (base, has_index): (PropertyMapKeys<'_>, bool) =
            match self.inner.as_ref().map(PropertyStore::view) {
                None => (PropertyMapKeys::Plain([].iter()), false),
                Some(StoreRef::Shaped { shape, .. }) => (
                    PropertyMapKeys::Plain(shape.keys().iter()),
                    shape.has_index_key,
                ),
                Some(StoreRef::Dict(dict)) => (
                    PropertyMapKeys::Entries(dict.entries.iter()),
                    dict.has_index_key,
                ),
            };
        if !has_index {
            return base;
        }
        let mut indexed: Vec<(u32, &JsString)> = Vec::new();
        let mut named: Vec<&JsString> = Vec::with_capacity(self.len());
        for key in base {
            match index_key_value(key.as_str()) {
                Some(index) => indexed.push((index, key)),
                None => named.push(key),
            }
        }
        indexed.sort_by_key(|(index, _)| *index);
        let mut ordered: Vec<&JsString> = indexed.into_iter().map(|(_, key)| key).collect();
        ordered.extend(named);
        PropertyMapKeys::Ordered(ordered.into_iter())
    }

    /// May contain an internal `__w3cos_` key (accessor halves). `false`
    /// is exact: no own key starts with [`MARKER_PREFIX`].
    #[inline]
    pub(crate) fn may_have_marker_key(&self) -> bool {
        match self.inner.as_ref().map(PropertyStore::view) {
            None => false,
            Some(StoreRef::Shaped { shape, .. }) => shape.has_marker_key,
            Some(StoreRef::Dict(_)) => true,
        }
    }

    pub(crate) fn any_key(&self, mut f: impl FnMut(&JsString) -> bool) -> bool {
        match self.inner.as_ref().map(PropertyStore::view) {
            None => false,
            Some(StoreRef::Shaped { shape, .. }) => shape.keys().iter().any(&mut f),
            Some(StoreRef::Dict(dict)) => dict.entries.iter().any(|(key, _)| f(key)),
        }
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) enum PropertyMapKeys<'a> {
    Plain(std::slice::Iter<'a, JsString>),
    Entries(std::slice::Iter<'a, (JsString, Value)>),
    Ordered(std::vec::IntoIter<&'a JsString>),
}

impl<'a> Iterator for PropertyMapKeys<'a> {
    type Item = &'a JsString;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            PropertyMapKeys::Plain(iter) => iter.next(),
            PropertyMapKeys::Entries(iter) => iter.next().map(|(key, _)| key),
            PropertyMapKeys::Ordered(iter) => iter.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(map: &PropertyMap) -> Vec<String> {
        map.keys().map(|k| k.as_str().to_string()).collect()
    }

    fn build(keys: &[&str]) -> PropertyMap {
        let mut map = PropertyMap::new();
        for (i, key) in keys.iter().enumerate() {
            map.insert_str(key, Value::Number(i as f64));
        }
        map
    }

    #[test]
    fn shaped_store_is_compact_and_small_objects_hold_exact_slots() {
        // A shape-backed store is one allocation: a 16-byte header (shape
        // pointer, len, capacity) followed by exactly the slots it needs.
        assert_eq!(std::mem::size_of::<PropertyMap>(), 8);
        assert_eq!(std::mem::size_of::<ShapedHeader>(), 16);
        let mut one = PropertyMap::new();
        one.insert_str("constructor", Value::Number(1.0));
        assert_eq!(
            one.heap_bytes(),
            std::mem::size_of::<ShapedHeader>() + std::mem::size_of::<Value>()
        );
        let mut grown = PropertyMap::new();
        for (index, key) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            grown.insert_str(key, Value::Number(index as f64));
        }
        assert!(grown.is_compact());
        assert_eq!(grown.len(), 5);
        assert_eq!(grown.get("e").map(|v| v.to_number()), Some(4.0));
        assert_eq!(grown.get("a").map(|v| v.to_number()), Some(0.0));
        grown.remove("c");
        assert!(grown.is_dictionary());
        assert_eq!(grown.get("d").map(|v| v.to_number()), Some(3.0));
        assert!(grown.get("c").is_none());
    }

    #[test]
    fn transition_chains_share_one_key_buffer_and_branches_copy() {
        let names: Vec<String> = (0..20).map(|i| format!("chain_share_key_{i}")).collect();
        let mut chain = vec![root_shape()];
        for name in &names {
            let next = chain.last().unwrap().transition(name).expect("budget");
            chain.push(next);
        }
        // Every shape sees exactly its own prefix of keys.
        for (len, shape) in chain.iter().enumerate() {
            let keys: Vec<&str> = shape.keys().iter().map(JsString::as_str).collect();
            let expected: Vec<&str> = names[..len].iter().map(String::as_str).collect();
            assert_eq!(keys, expected);
            assert_eq!(shape.lookup(&names[0]), (len > 0).then_some(0));
        }
        // Shapes 4..=4 share a buffer of capacity 4, 5..=8 one of 8, 9..=16
        // one of 16: a chain of n keys stores under 2n keys, not n^2/2.
        assert!(Rc::ptr_eq(&chain[3].key_buf, &chain[4].key_buf));
        assert!(Rc::ptr_eq(&chain[5].key_buf, &chain[8].key_buf));
        assert!(Rc::ptr_eq(&chain[9].key_buf, &chain[16].key_buf));
        assert!(!Rc::ptr_eq(&chain[4].key_buf, &chain[5].key_buf));
        // A second child of a non-tip shape gets its own copy of the prefix
        // and leaves the original chain untouched.
        let branch = chain[2].transition("chain_share_branch").expect("budget");
        assert!(!Rc::ptr_eq(&branch.key_buf, &chain[2].key_buf));
        let branch_keys: Vec<&str> = branch.keys().iter().map(JsString::as_str).collect();
        assert_eq!(
            branch_keys,
            vec![names[0].as_str(), names[1].as_str(), "chain_share_branch"]
        );
        assert_eq!(chain[3].keys()[2].as_str(), names[2]);
        // Re-walking finds the same shapes.
        assert!(Rc::ptr_eq(
            &chain[2].transition(&names[2]).unwrap(),
            &chain[3]
        ));
    }

    #[test]
    fn wide_shapes_index_only_once_they_keep_being_searched() {
        let keys: Vec<String> = (0..40).map(|i| format!("wide_index_key_{i}")).collect();
        let mut map = PropertyMap::new();
        for (i, key) in keys.iter().enumerate() {
            assert!(map.get(key).is_none(), "fill-path miss");
            map.insert_str(key, Value::Number(i as f64));
        }
        let shape = match map.inner.as_ref().map(PropertyStore::view) {
            Some(StoreRef::Shaped { shape, .. }) => Rc::clone(shape),
            _ => panic!("40 keys stay shape-backed"),
        };
        // Every prefix shape was searched once on the way: none indexed.
        let mut prefix = root_shape();
        for key in &keys {
            prefix = prefix.transition(key).expect("existing transition");
            assert!(prefix.index.get().is_none(), "{} keys", prefix.keys().len());
        }
        assert!(Rc::ptr_eq(&prefix, &shape));
        // Lookups stay correct before and after the index exists.
        for round in 0..3 {
            for (i, key) in keys.iter().enumerate() {
                assert_eq!(
                    map.get(key).map(Value::to_number),
                    Some(i as f64),
                    "round {round}"
                );
            }
            assert!(map.get("wide_index_key_absent").is_none());
        }
        assert!(
            shape.index.get().is_some(),
            "a shape that keeps being searched indexes"
        );
    }

    #[test]
    fn empty_map_is_null_and_compact() {
        let map = PropertyMap::new();
        assert!(map.is_empty());
        assert!(map.is_compact());
        assert_eq!(map.heap_bytes(), 0);
        assert_eq!(map.shape_id(), Some(ROOT_SHAPE_ID));
        assert_eq!(
            std::mem::size_of::<PropertyMap>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn same_key_order_shares_one_shape() {
        let a = build(&["x", "y", "z"]);
        let b = build(&["x", "y", "z"]);
        let c = build(&["y", "x", "z"]);
        assert!(a.is_compact() && b.is_compact() && c.is_compact());
        assert_eq!(a.shape_id(), b.shape_id());
        assert_ne!(a.shape_id(), c.shape_id());
        // Prefix shapes are the transition tree's interior nodes.
        let prefix = build(&["x", "y"]);
        assert_ne!(prefix.shape_id(), a.shape_id());
        assert_eq!(prefix.shape_id(), build(&["x", "y"]).shape_id());
        assert_eq!(a.shaped_slot("z").map(|(_, slot)| slot), Some(2));
        assert_eq!(c.shaped_slot("z").map(|(_, slot)| slot), Some(2));
        assert_eq!(c.shaped_slot("x").map(|(_, slot)| slot), Some(1));
    }

    #[test]
    fn overwrite_keeps_shape_and_insertion_order() {
        let mut map = build(&["a", "b"]);
        let shape = map.shape_id();
        assert!(map.insert_str("a", Value::Number(3.0)).is_some());
        assert_eq!(map.shape_id(), shape);
        assert_eq!(keys(&map), vec!["a", "b"]);
        assert_eq!(map.get("a").map(Value::to_number), Some(3.0));
    }

    #[test]
    fn wide_shapes_use_index_and_stay_compact() {
        let names: Vec<String> = (0..MAX_SHAPE_KEYS).map(|i| format!("k{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut map = build(&refs);
        assert!(map.is_compact());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(map.get(name).map(Value::to_number), Some(i as f64));
        }
        // One past the limit goes to dictionary mode, order preserved.
        map.insert_str("overflow", Value::Bool(true));
        assert!(map.is_dictionary());
        assert_eq!(map.shape_id(), None);
        assert_eq!(keys(&map).last().map(String::as_str), Some("overflow"));
        assert_eq!(keys(&map).len(), MAX_SHAPE_KEYS + 1);
    }

    #[test]
    fn delete_goes_to_dictionary_and_keeps_order() {
        let mut map = build(&["a", "b", "c"]);
        assert!(map.remove("missing").is_none());
        assert!(map.is_compact(), "a failed delete keeps the shape");
        assert_eq!(map.remove("b").map(|v| v.to_number()), Some(1.0));
        assert!(map.is_dictionary());
        assert_eq!(map.shape_id(), None);
        assert_eq!(keys(&map), vec!["a", "c"]);
        map.insert_str("b", Value::Number(9.0));
        assert_eq!(keys(&map), vec!["a", "c", "b"]);
        assert!(map.shaped_slot("a").is_none());
    }

    #[test]
    fn remove_back_to_empty_drops_box() {
        let mut map = PropertyMap::new();
        map.insert(JsString::intern("only"), Value::Number(1.0));
        assert!(map.is_compact());
        assert!(map.remove("only").is_some());
        assert!(map.is_empty());
        assert_eq!(map.heap_bytes(), 0);
        assert_eq!(map.shape_id(), Some(ROOT_SHAPE_ID));
    }

    #[test]
    fn integer_keys_enumerate_first_in_ascending_order() {
        let map = build(&["b", "10", "a", "2", "01", "0", "4294967295"]);
        // "01" and 2^32-1 are not array indices: they keep insertion order.
        assert_eq!(
            keys(&map),
            vec!["0", "2", "10", "b", "a", "01", "4294967295"]
        );
        let mut dict = map.clone();
        dict.remove("a");
        assert!(dict.is_dictionary());
        assert_eq!(keys(&dict), vec!["0", "2", "10", "b", "01", "4294967295"]);
    }

    #[test]
    fn shape_keys_survive_page_reset() {
        let map = build(&["survives_reset_key"]);
        crate::page_arena::reset();
        assert_eq!(keys(&map), vec!["survives_reset_key"]);
        assert_eq!(
            map.get("survives_reset_key").map(Value::to_number),
            Some(0.0)
        );
    }

    #[test]
    fn ic_slot_accessors_check_shape() {
        let mut map = build(&["x", "y"]);
        let (shape, slot) = map.shaped_slot("y").expect("shaped");
        assert_eq!(
            map.slot_if_shape(shape, slot).map(Value::to_number),
            Some(1.0)
        );
        *map.slot_mut_if_shape(shape, slot).expect("hit") = Value::Number(5.0);
        assert_eq!(map.get("y").map(Value::to_number), Some(5.0));
        map.insert_str("z", Value::Null);
        assert!(
            map.slot_if_shape(shape, slot).is_none(),
            "shape changed -> miss"
        );
        map.remove("x");
        assert!(
            map.slot_mut_if_shape(shape, slot).is_none(),
            "dictionary -> miss"
        );
    }

    #[test]
    fn marker_keys_are_tracked() {
        assert!(!build(&["a", "b"]).may_have_marker_key());
        assert!(build(&["a", "__w3cos_setter_b"]).may_have_marker_key());
    }

    #[test]
    fn from_hashmap_builds_sorted_shape_or_dictionary() {
        let small: HashMap<JsString, Value> = ["b", "a", "c"]
            .iter()
            .enumerate()
            .map(|(i, k)| (JsString::intern(k), Value::Number(i as f64)))
            .collect();
        let map = PropertyMap::from_interned_hashmap(small);
        assert!(map.is_compact());
        assert_eq!(keys(&map), vec!["a", "b", "c"]);
        assert_eq!(map.shape_id(), build(&["a", "b", "c"]).shape_id());
        assert_eq!(map.get("b").map(Value::to_number), Some(0.0));
        let wide: HashMap<JsString, Value> = (0..MAX_SHAPE_KEYS + 3)
            .map(|i| {
                (
                    JsString::intern(&format!("w{i:03}")),
                    Value::Number(i as f64),
                )
            })
            .collect();
        let map = PropertyMap::from_interned_hashmap(wide);
        assert!(map.is_dictionary());
        assert_eq!(map.len(), MAX_SHAPE_KEYS + 3);
        for i in 0..MAX_SHAPE_KEYS + 3 {
            assert_eq!(
                map.get(&format!("w{i:03}")).map(Value::to_number),
                Some(i as f64)
            );
        }
        assert_eq!(keys(&map).first().map(String::as_str), Some("w000"));
    }

    #[test]
    fn shape_ids_are_unique_across_threads() {
        let here = build(&["thread_unique_key"]).shape_id();
        let there = std::thread::spawn(|| build(&["thread_unique_key"]).shape_id())
            .join()
            .expect("thread");
        assert_ne!(here, there);
        assert_ne!(here, Some(ROOT_SHAPE_ID));
    }

    #[test]
    fn dictionary_index_tracks_inserts_and_removals_past_the_linear_limit() {
        let names: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
        let mut map = PropertyMap::new();
        for (i, name) in names.iter().enumerate() {
            map.insert_str(name, Value::Number(i as f64));
            if i == 3 {
                // A delete forces dictionary mode while the map is small.
                map.remove("k0");
                map.insert_str("k0", Value::Number(0.0));
            }
        }
        assert!(map.is_dictionary());
        assert_eq!(map.len(), 40);
        for (i, name) in names.iter().enumerate() {
            assert_eq!(
                map.get(name).map(Value::to_number),
                Some(i as f64),
                "{name}"
            );
        }
        // Remove from the middle, the front and the back; positions shift.
        for gone in ["k20", "k1", "k39"] {
            assert!(map.remove(gone).is_some());
            assert!(map.get(gone).is_none());
        }
        assert_eq!(map.len(), 37);
        for (i, name) in names.iter().enumerate() {
            if ["k20", "k1", "k39"].contains(&name.as_str()) {
                continue;
            }
            assert_eq!(
                map.get(name).map(Value::to_number),
                Some(i as f64),
                "{name}"
            );
        }
        let order = keys(&map);
        assert_eq!(order[0], "k2");
        assert_eq!(order.iter().position(|k| k == "k0"), Some(2));
        assert_eq!(order.last().map(String::as_str), Some("k38"));
        // Shrinking back under the limit drops the index; lookups still work.
        for name in &names[5..39] {
            map.remove(name);
        }
        assert!(map.get("k2").is_some() && map.get("k0").is_some() && map.get("k30").is_none());
        assert_eq!(keys(&map), vec!["k2", "k3", "k0", "k4"]);
    }

    #[test]
    fn wide_literal_dictionaries_keep_sorted_key_order() {
        let mut source = HashMap::new();
        for i in 0..(MAX_SHAPE_KEYS + 10) {
            source.insert(
                JsString::intern(&format!("p{i:03}")),
                Value::Number(i as f64),
            );
        }
        let map = PropertyMap::from_interned_hashmap(source);
        assert!(map.is_dictionary());
        let order = keys(&map);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        assert_eq!(order.len(), MAX_SHAPE_KEYS + 10);
        assert_eq!(map.get("p070").map(Value::to_number), Some(70.0));
    }

    #[test]
    fn shaped_buffers_grow_clone_and_convert_without_losing_values() {
        let names: Vec<String> = (0..40).map(|i| format!("sb_key_{i}")).collect();
        let mut map = PropertyMap::new();
        for (i, name) in names.iter().enumerate() {
            map.insert_str(name, Value::string(&format!("v{i}")));
            assert_eq!(map.len(), i + 1);
            assert!(map.is_compact());
            let cap = (i + 1).next_power_of_two();
            assert_eq!(
                map.heap_bytes(),
                std::mem::size_of::<ShapedHeader>() + cap * std::mem::size_of::<Value>()
            );
        }
        let copy = map.clone();
        // Overwrite in place, then leave the shape tree.
        map.insert_str("sb_key_3", Value::Number(3.0));
        map.remove("sb_key_0");
        assert!(map.is_dictionary());
        assert_eq!(map.get("sb_key_3").map(Value::to_number), Some(3.0));
        assert_eq!(
            map.get("sb_key_39").map(|v| v.to_js_string()),
            Some("v39".to_string())
        );
        assert!(map.get("sb_key_0").is_none());
        // The clone kept its own slots.
        assert!(copy.is_compact());
        assert_eq!(copy.len(), 40);
        assert_eq!(
            copy.get("sb_key_3").map(|v| v.to_js_string()),
            Some("v3".to_string())
        );
        assert_eq!(
            copy.get("sb_key_0").map(|v| v.to_js_string()),
            Some("v0".to_string())
        );
        // Removing the only key empties the map.
        let mut one = PropertyMap::new();
        one.insert_str("only", Value::Number(7.0));
        assert_eq!(one.remove("only").map(|v| v.to_number()), Some(7.0));
        assert!(one.is_empty());
        assert_eq!(one.heap_bytes(), 0);
    }
}
