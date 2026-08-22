use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::{Deref, Index, IndexMut};
use std::rc::{Rc, Weak};
use std::slice::SliceIndex;

use crate::heap::{HeapAllocation, HeapKind};
use crate::js_string::JsString;

/// JavaScript-compatible dynamic value type.
///
/// First-cut ABI: Undefined / Null / Bool / Number and interned short
/// strings are a Copy tagged word ([`Immediate`], NaN-box). Clone of
/// those immediates is a register move of the 8-byte word and does not
/// touch `Rc`.
///
/// Long `Rc<str>` strings, host/DOM objects, host arrays, and host/DOM
/// callables stay as a remaining enum of pointers / `Rc`. Page-local objects
/// created via [`Value::object`] / literals, constructor / literal arrays
/// created via [`Value::array`], and ordinary JS closures created via
/// [`Value::function`] / AOT `CreateClosure` are `u32` handles packed in
/// [`Immediate`] (clone is a register move). `array_hole` stays a host `Rc`
/// object. Host/DOM callables (`Value::callable`, jsdom call slots) stay
/// `Value::Object(Rc)` / `Value::Function(Rc)`. Symbols remain interned
/// strings (`__w3cos_symbol_…`).
///
/// Constructors `Value::Undefined`, `Value::Null`, `Value::Bool`, and
/// `Value::Number` are unchanged so AOT emission (`num_regs` / `bool_regs`
/// boxing) stays valid. `String` / `Array` / `Object` / `Function`
/// variant names are unchanged.
///
/// Core heap values are thread-confined (`Rc` + thread-local heap
/// counters), so string intern is thread-local as well.
#[derive(Clone)]
pub enum Value {
    /// NaN-boxed undefined / null / bool / number / interned short string.
    /// Prefer `Value::Undefined` / `Value::Bool` / `Value::Number` /
    /// `Value::string` constructors over matching this arm.
    Imm(Immediate),
    String(JsString),
    Array(Rc<RefCell<ArrayStorage>>),
    Object(Rc<RefCell<crate::JsObject>>),
    Function(Rc<FunctionData>),
}

impl Default for Value {
    #[inline]
    fn default() -> Self {
        Value::Undefined
    }
}

/// Copy tagged word for JS immediates (NaN-box).
///
/// Encoding (`u64` bits, `#[repr(transparent)]` so `transmute` to/from
/// `u64` is the identity):
///
/// - **Number:** IEEE-754 bits. Every NaN is canonicalized to
///   `0x7FF8_0000_0000_0000` so payloads never collide with tags.
/// - **Tagged** quiet-NaN with low-4-bit tag `QNAN | tag | (payload << 4)`:
///   - `1` undefined
///   - `2` null
///   - `3` false
///   - `4` true
///   - `5` interned page-arena string (`u32` handle payload)
///   - `6` page-arena object (`u32` handle payload)
///   - `7` page-arena array (`u32` handle payload)
///   - `8` page-arena function (`u32` handle payload)
///
/// Long `Rc<str>` and heap pointers are **not** in this word (remaining
/// `Value` enum). Tagged payloads use a 4-bit tag (`HANDLE_SHIFT = 4`).
#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct Immediate(u64);

impl Immediate {
    const QNAN: u64 = 0x7FF8_0000_0000_0000;
    const TAG_NAN: u64 = 0;
    const TAG_UNDEF: u64 = 1;
    const TAG_NULL: u64 = 2;
    const TAG_FALSE: u64 = 3;
    const TAG_TRUE: u64 = 4;
    const TAG_STR: u64 = 5;
    const TAG_OBJ: u64 = 6;
    const TAG_ARR: u64 = 7;
    const TAG_FN: u64 = 8;
    const TAG_MASK: u64 = 0xF;
    const HANDLE_SHIFT: u64 = 4;

    pub const UNDEFINED: Self = Self(Self::QNAN | Self::TAG_UNDEF);
    pub const NULL: Self = Self(Self::QNAN | Self::TAG_NULL);

    #[inline]
    pub const fn from_bool(b: bool) -> Self {
        if b {
            Self(Self::QNAN | Self::TAG_TRUE)
        } else {
            Self(Self::QNAN | Self::TAG_FALSE)
        }
    }

    #[inline]
    pub fn from_number(n: f64) -> Self {
        if n.is_nan() {
            Self(Self::QNAN | Self::TAG_NAN)
        } else {
            Self(n.to_bits())
        }
    }

    /// Pack a page-arena intern handle into the tagged word.
    #[inline]
    pub const fn from_interned_string(handle: u32) -> Self {
        Self(Self::QNAN | Self::TAG_STR | ((handle as u64) << Self::HANDLE_SHIFT))
    }

    /// Pack a page-arena object handle into the tagged word.
    #[inline]
    pub const fn from_object_handle(handle: u32) -> Self {
        Self(Self::QNAN | Self::TAG_OBJ | ((handle as u64) << Self::HANDLE_SHIFT))
    }

    /// Pack a page-arena array handle into the tagged word.
    #[inline]
    pub const fn from_array_handle(handle: u32) -> Self {
        Self(Self::QNAN | Self::TAG_ARR | ((handle as u64) << Self::HANDLE_SHIFT))
    }

    /// Pack a page-arena function handle into the tagged word.
    #[inline]
    pub const fn from_function_handle(handle: u32) -> Self {
        Self(Self::QNAN | Self::TAG_FN | ((handle as u64) << Self::HANDLE_SHIFT))
    }

    /// Raw tagged bits. Documented transmute target.
    #[inline]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[inline]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    #[inline]
    const fn is_tagged(self) -> bool {
        (self.0 & Self::QNAN) == Self::QNAN
    }

    #[inline]
    const fn tag(self) -> u64 {
        self.0 & Self::TAG_MASK
    }

    #[inline]
    pub const fn is_undefined(self) -> bool {
        self.0 == Self::UNDEFINED.0
    }

    #[inline]
    pub const fn is_null(self) -> bool {
        self.0 == Self::NULL.0
    }

    #[inline]
    pub const fn is_bool(self) -> bool {
        self.is_tagged() && (self.tag() == Self::TAG_FALSE || self.tag() == Self::TAG_TRUE)
    }

    #[inline]
    pub const fn is_number(self) -> bool {
        !self.is_tagged() || self.tag() == Self::TAG_NAN
    }

    #[inline]
    pub const fn as_bool(self) -> Option<bool> {
        if !self.is_tagged() {
            return None;
        }
        match self.tag() {
            Self::TAG_FALSE => Some(false),
            Self::TAG_TRUE => Some(true),
            _ => None,
        }
    }

    #[inline]
    pub fn as_number(self) -> Option<f64> {
        if self.is_number() {
            Some(f64::from_bits(self.0))
        } else {
            None
        }
    }

    #[inline]
    pub const fn is_interned_string(self) -> bool {
        self.is_tagged() && self.tag() == Self::TAG_STR
    }

    #[inline]
    pub const fn interned_handle(self) -> Option<u32> {
        if self.is_interned_string() {
            Some((self.0 >> Self::HANDLE_SHIFT) as u32)
        } else {
            None
        }
    }

    #[inline]
    pub fn as_js_string(self) -> Option<JsString> {
        self.interned_handle().and_then(JsString::from_page_handle)
    }

    #[inline]
    pub const fn is_object_handle(self) -> bool {
        self.is_tagged() && self.tag() == Self::TAG_OBJ
    }

    #[inline]
    pub const fn object_handle(self) -> Option<u32> {
        if self.is_object_handle() {
            Some((self.0 >> Self::HANDLE_SHIFT) as u32)
        } else {
            None
        }
    }

    #[inline]
    pub const fn is_array_handle(self) -> bool {
        self.is_tagged() && self.tag() == Self::TAG_ARR
    }

    #[inline]
    pub const fn array_handle(self) -> Option<u32> {
        if self.is_array_handle() {
            Some((self.0 >> Self::HANDLE_SHIFT) as u32)
        } else {
            None
        }
    }

    #[inline]
    pub const fn is_function_handle(self) -> bool {
        self.is_tagged() && self.tag() == Self::TAG_FN
    }

    #[inline]
    pub const fn function_handle(self) -> Option<u32> {
        if self.is_function_handle() {
            Some((self.0 >> Self::HANDLE_SHIFT) as u32)
        } else {
            None
        }
    }
}

impl PartialEq for Immediate {
    fn eq(&self, other: &Self) -> bool {
        match (self.as_number(), other.as_number()) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.0 == other.0,
            _ => false,
        }
    }
}

impl Value {
    /// Unit-like constructor kept so AOT / host code can write `Value::Undefined`.
    #[allow(non_upper_case_globals)]
    pub const Undefined: Value = Value::Imm(Immediate::UNDEFINED);
    /// Unit-like constructor kept so AOT / host code can write `Value::Null`.
    #[allow(non_upper_case_globals)]
    pub const Null: Value = Value::Imm(Immediate::NULL);

    /// Tuple-like constructor kept so AOT can write `Value::Bool(self.bool_regs[i])`.
    #[allow(non_snake_case)]
    #[inline]
    pub const fn Bool(b: bool) -> Self {
        Value::Imm(Immediate::from_bool(b))
    }

    /// Tuple-like constructor kept so AOT can write `Value::Number(self.num_regs[i])`.
    #[allow(non_snake_case)]
    #[inline]
    pub fn Number(n: f64) -> Self {
        Value::Imm(Immediate::from_number(n))
    }

    #[inline]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Imm(imm) => imm.as_bool(),
            _ => None,
        }
    }

    #[inline]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Imm(imm) => imm.as_number(),
            _ => None,
        }
    }

    #[inline]
    pub fn as_immediate(&self) -> Option<Immediate> {
        match self {
            Value::Imm(imm) => Some(*imm),
            _ => None,
        }
    }

    /// Interned page-arena handle or long heap `JsString`.
    #[inline]
    pub fn as_js_string(&self) -> Option<JsString> {
        match self {
            Value::Imm(imm) => imm.as_js_string(),
            Value::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Pack interned short strings into [`Immediate`]; long `Rc<str>` stay
    /// on the `String` arm.
    #[inline]
    pub fn from_js_string(s: JsString) -> Self {
        if let Some(handle) = s.page_handle() {
            Value::Imm(Immediate::from_interned_string(handle))
        } else {
            Value::String(s)
        }
    }

    /// Page-arena object handle or host `Rc` object.
    #[inline]
    pub fn as_object(&self) -> Option<JsObjectRef> {
        match self {
            Value::Imm(imm) => imm.object_handle().map(crate::page_arena::get_object),
            Value::Object(object) => Some(JsObjectRef::from_host(Rc::clone(object))),
            _ => None,
        }
    }

    /// Packed page-arena object handle, when this value is not a host `Rc`.
    #[inline]
    pub fn object_handle(&self) -> Option<u32> {
        match self {
            Value::Imm(imm) => imm.object_handle(),
            _ => None,
        }
    }

    fn object_identity_eq(left: &Value, right: &Value) -> bool {
        match (left.object_handle(), right.object_handle()) {
            (Some(a), Some(b)) => a == b,
            _ => match (left.as_object(), right.as_object()) {
                (Some(a), Some(b)) => a.ptr_eq(&b),
                _ => false,
            },
        }
    }

    /// Page-arena array handle or host `Rc` array.
    #[inline]
    pub fn as_array(&self) -> Option<JsArrayRef> {
        match self {
            Value::Imm(imm) => imm.array_handle().map(crate::page_arena::get_array),
            Value::Array(array) => Some(JsArrayRef::from_host(Rc::clone(array))),
            _ => None,
        }
    }

    /// Packed page-arena array handle, when this value is not a host `Rc`.
    #[inline]
    pub fn array_handle(&self) -> Option<u32> {
        match self {
            Value::Imm(imm) => imm.array_handle(),
            _ => None,
        }
    }

    fn array_identity_eq(left: &Value, right: &Value) -> bool {
        match (left.array_handle(), right.array_handle()) {
            (Some(a), Some(b)) => a == b,
            _ => match (left.as_array(), right.as_array()) {
                (Some(a), Some(b)) => a.ptr_eq(&b),
                _ => false,
            },
        }
    }

    /// Page-arena function handle or host `Rc` function.
    #[inline]
    pub fn as_function(&self) -> Option<JsFunction> {
        match self {
            Value::Imm(imm) => imm.function_handle().map(crate::page_arena::get_function),
            Value::Function(function) => Some(JsFunction::from_host(Rc::clone(function))),
            _ => None,
        }
    }

    /// Packed page-arena function handle, when this value is not a host `Rc`.
    #[inline]
    pub fn function_handle(&self) -> Option<u32> {
        match self {
            Value::Imm(imm) => imm.function_handle(),
            _ => None,
        }
    }

    fn function_identity_eq(left: &Value, right: &Value) -> bool {
        match (left.function_handle(), right.function_handle()) {
            (Some(a), Some(b)) => a == b,
            _ => match (left.as_function(), right.as_function()) {
                (Some(a), Some(b)) => a.ptr_eq(&b),
                _ => false,
            },
        }
    }
}

/// Shared backing storage for [`Value::Array`].
///
/// Construction remains centralized through [`Value::array`] so heap
/// accounting cannot be bypassed.
pub struct ArrayStorage {
    values: Vec<Value>,
    allocation: HeapAllocation,
}

impl ArrayStorage {
    pub fn new(values: Vec<Value>) -> Self {
        let allocation = HeapAllocation::new(
            HeapKind::Array,
            std::mem::size_of::<Self>().saturating_add(
                values
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>()),
            ),
        );
        Self { values, allocation }
    }

    pub(crate) fn refresh_heap_accounting(&self) {
        self.allocation.set_bytes(
            std::mem::size_of::<Self>().saturating_add(
                self.values
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>()),
            ),
        );
    }

    pub fn replace_values(&mut self, values: Vec<Value>) {
        self.values = values;
        self.refresh_heap_accounting();
    }

    pub(crate) fn push(&mut self, value: Value) {
        self.values.push(value);
        self.refresh_heap_accounting();
    }

    pub(crate) fn pop(&mut self) -> Option<Value> {
        self.values.pop()
    }

    pub(crate) fn remove(&mut self, index: usize) -> Value {
        self.values.remove(index)
    }

    pub(crate) fn split_off(&mut self, at: usize) -> Vec<Value> {
        self.values.split_off(at)
    }

    pub(crate) fn resize_with(&mut self, new_len: usize, f: impl FnMut() -> Value) {
        self.values.resize_with(new_len, f);
        self.refresh_heap_accounting();
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
        self.values.get_mut(index)
    }

    pub(crate) fn clear(&mut self) {
        self.values.clear();
    }

    pub(crate) fn reverse(&mut self) {
        self.values.reverse();
    }
}

impl Deref for ArrayStorage {
    type Target = Vec<Value>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl Extend<Value> for ArrayStorage {
    fn extend<T: IntoIterator<Item = Value>>(&mut self, iter: T) {
        self.values.extend(iter);
        self.refresh_heap_accounting();
    }
}

impl<I> Index<I> for ArrayStorage
where
    I: SliceIndex<[Value]>,
{
    type Output = I::Output;

    fn index(&self, index: I) -> &Self::Output {
        &self.values[index]
    }
}

impl<I> IndexMut<I> for ArrayStorage
where
    I: SliceIndex<[Value]>,
{
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        &mut self.values[index]
    }
}

impl fmt::Debug for ArrayStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.values.fmt(formatter)
    }
}

/// A JS object: interned page-local handle, or host `Rc`.
///
/// Interned `Clone` copies a `u32` handle (plus a cached payload pointer).
/// It does not `Rc::clone` the object. Host clones clone one
/// `Rc<RefCell<JsObject>>`. Lookups use the cached slot pointer so they
/// do not hold the page-arena `RefCell` (nested `Value::object` would
/// otherwise `BorrowMutError`).
pub struct JsObjectRef {
    repr: JsObjectRepr,
}

enum JsObjectRepr {
    Interned {
        handle: u32,
        epoch: u32,
        ptr: *const RefCell<crate::JsObject>,
        _thread: PhantomData<Rc<()>>,
    },
    Host(Rc<RefCell<crate::JsObject>>),
}

pub(crate) enum WeakJsObject {
    Interned { handle: u32, epoch: u32 },
    Host(Weak<RefCell<crate::JsObject>>),
}

/// A JS array: interned page-local handle, or host `Rc`.
///
/// Interned `Clone` copies a `u32` handle (plus a cached payload pointer).
/// It does not `Rc::clone` the storage. Host clones clone one
/// `Rc<RefCell<ArrayStorage>>`. `array_hole` stays a host object.
pub struct JsArrayRef {
    repr: JsArrayRepr,
}

enum JsArrayRepr {
    Interned {
        handle: u32,
        epoch: u32,
        ptr: *const RefCell<ArrayStorage>,
        _thread: PhantomData<Rc<()>>,
    },
    Host(Rc<RefCell<ArrayStorage>>),
}

pub(crate) enum WeakJsArray {
    Interned { handle: u32, epoch: u32 },
    Host(Weak<RefCell<ArrayStorage>>),
}

impl Clone for JsObjectRef {
    fn clone(&self) -> Self {
        match self.repr {
            JsObjectRepr::Interned {
                handle,
                epoch,
                ptr,
                _thread,
            } => Self {
                repr: JsObjectRepr::Interned {
                    handle,
                    epoch,
                    ptr,
                    _thread,
                },
            },
            JsObjectRepr::Host(ref rc) => Self {
                repr: JsObjectRepr::Host(Rc::clone(rc)),
            },
        }
    }
}

impl JsObjectRef {
    pub(crate) fn from_interned(
        handle: u32,
        epoch: u32,
        ptr: *const RefCell<crate::JsObject>,
    ) -> Self {
        Self {
            repr: JsObjectRepr::Interned {
                handle,
                epoch,
                ptr,
                _thread: PhantomData,
            },
        }
    }

    pub(crate) fn from_host(object: Rc<RefCell<crate::JsObject>>) -> Self {
        Self {
            repr: JsObjectRepr::Host(object),
        }
    }

    /// Resolve payload without holding the page-arena `RefCell`.
    fn cell(&self) -> &RefCell<crate::JsObject> {
        match self.repr {
            JsObjectRepr::Interned { epoch, ptr, .. } => {
                if epoch != crate::page_arena::current_epoch() || ptr.is_null() {
                    panic!("page object handle used after reset_bridge");
                }
                // Safety: `ptr` names a `RefCell<JsObject>` box in the
                // current page object table. `reset` bumps the epoch before
                // dropping those boxes. Callers must not hold the reference
                // across a page reset.
                unsafe { &*ptr }
            }
            JsObjectRepr::Host(ref rc) => rc,
        }
    }

    /// Restore an interned `Imm` handle or a host `Value::Object`.
    pub(crate) fn as_value(&self) -> Value {
        match self.repr {
            JsObjectRepr::Interned { handle, epoch, .. } => {
                if epoch != crate::page_arena::current_epoch() {
                    panic!("page object handle used after reset_bridge");
                }
                Value::Imm(Immediate::from_object_handle(handle))
            }
            JsObjectRepr::Host(ref rc) => Value::Object(Rc::clone(rc)),
        }
    }

    /// Object identity: two interned handles name the same slot, or two
    /// host objects share the same `JsObject` allocation.
    pub fn ptr_eq(&self, other: &JsObjectRef) -> bool {
        match (&self.repr, &other.repr) {
            (
                JsObjectRepr::Interned {
                    handle: a,
                    epoch: ea,
                    ..
                },
                JsObjectRepr::Interned {
                    handle: b,
                    epoch: eb,
                    ..
                },
            ) => a == b && ea == eb,
            (JsObjectRepr::Host(a), JsObjectRepr::Host(b)) => Rc::ptr_eq(a, b),
            _ => std::ptr::eq(self.cell(), other.cell()),
        }
    }

    /// Stable identity address for this object value.
    pub fn identity(&self) -> usize {
        self.cell() as *const RefCell<crate::JsObject> as usize
    }

    #[cfg(test)]
    pub(crate) fn interned_handle(&self) -> Option<u32> {
        match self.repr {
            JsObjectRepr::Interned { handle, .. } => Some(handle),
            JsObjectRepr::Host(_) => None,
        }
    }

    /// Strong count of the host `Rc`, when this is not interned.
    #[cfg(test)]
    pub(crate) fn host_strong_count(&self) -> Option<usize> {
        match &self.repr {
            JsObjectRepr::Host(rc) => Some(Rc::strong_count(rc)),
            JsObjectRepr::Interned { .. } => None,
        }
    }

    pub(crate) fn downgrade(&self) -> WeakJsObject {
        match self.repr {
            JsObjectRepr::Interned { handle, epoch, .. } => {
                WeakJsObject::Interned { handle, epoch }
            }
            JsObjectRepr::Host(ref rc) => WeakJsObject::Host(Rc::downgrade(rc)),
        }
    }
}

impl WeakJsObject {
    pub(crate) fn upgrade(&self) -> Option<JsObjectRef> {
        match self {
            WeakJsObject::Interned { handle, epoch } => {
                crate::page_arena::upgrade_object(*handle, *epoch)
            }
            WeakJsObject::Host(weak) => weak.upgrade().map(JsObjectRef::from_host),
        }
    }

    pub(crate) fn upgrade_value(&self) -> Option<Value> {
        self.upgrade().map(|object| object.as_value())
    }
}

impl Deref for JsObjectRef {
    type Target = RefCell<crate::JsObject>;

    fn deref(&self) -> &Self::Target {
        self.cell()
    }
}

impl Clone for JsArrayRef {
    fn clone(&self) -> Self {
        match self.repr {
            JsArrayRepr::Interned {
                handle,
                epoch,
                ptr,
                _thread,
            } => Self {
                repr: JsArrayRepr::Interned {
                    handle,
                    epoch,
                    ptr,
                    _thread,
                },
            },
            JsArrayRepr::Host(ref rc) => Self {
                repr: JsArrayRepr::Host(Rc::clone(rc)),
            },
        }
    }
}

impl JsArrayRef {
    pub(crate) fn from_interned(
        handle: u32,
        epoch: u32,
        ptr: *const RefCell<ArrayStorage>,
    ) -> Self {
        Self {
            repr: JsArrayRepr::Interned {
                handle,
                epoch,
                ptr,
                _thread: PhantomData,
            },
        }
    }

    pub(crate) fn from_host(array: Rc<RefCell<ArrayStorage>>) -> Self {
        Self {
            repr: JsArrayRepr::Host(array),
        }
    }

    /// Resolve payload without holding the page-arena `RefCell`.
    fn cell(&self) -> &RefCell<ArrayStorage> {
        match self.repr {
            JsArrayRepr::Interned { epoch, ptr, .. } => {
                if epoch != crate::page_arena::current_epoch() || ptr.is_null() {
                    panic!("page array handle used after reset_bridge");
                }
                // Safety: `ptr` names a `RefCell<ArrayStorage>` box in the
                // current page array table. `reset` bumps the epoch before
                // dropping those boxes. Callers must not hold the reference
                // across a page reset.
                unsafe { &*ptr }
            }
            JsArrayRepr::Host(ref rc) => rc,
        }
    }

    /// Restore an interned `Imm` handle or a host `Value::Array`.
    pub(crate) fn as_value(&self) -> Value {
        match self.repr {
            JsArrayRepr::Interned { handle, epoch, .. } => {
                if epoch != crate::page_arena::current_epoch() {
                    panic!("page array handle used after reset_bridge");
                }
                Value::Imm(Immediate::from_array_handle(handle))
            }
            JsArrayRepr::Host(ref rc) => Value::Array(Rc::clone(rc)),
        }
    }

    /// Array identity: two interned handles name the same slot, or two
    /// host arrays share the same `ArrayStorage` allocation.
    pub fn ptr_eq(&self, other: &JsArrayRef) -> bool {
        match (&self.repr, &other.repr) {
            (
                JsArrayRepr::Interned {
                    handle: a,
                    epoch: ea,
                    ..
                },
                JsArrayRepr::Interned {
                    handle: b,
                    epoch: eb,
                    ..
                },
            ) => a == b && ea == eb,
            (JsArrayRepr::Host(a), JsArrayRepr::Host(b)) => Rc::ptr_eq(a, b),
            _ => std::ptr::eq(self.cell(), other.cell()),
        }
    }

    /// Stable identity address for this array value.
    pub fn identity(&self) -> usize {
        self.cell() as *const RefCell<ArrayStorage> as usize
    }

    #[cfg(test)]
    pub(crate) fn interned_handle(&self) -> Option<u32> {
        match self.repr {
            JsArrayRepr::Interned { handle, .. } => Some(handle),
            JsArrayRepr::Host(_) => None,
        }
    }

    /// Strong count of the host `Rc`, when this is not interned.
    #[cfg(test)]
    pub(crate) fn host_strong_count(&self) -> Option<usize> {
        match &self.repr {
            JsArrayRepr::Host(rc) => Some(Rc::strong_count(rc)),
            JsArrayRepr::Interned { .. } => None,
        }
    }

    pub(crate) fn downgrade(&self) -> WeakJsArray {
        match self.repr {
            JsArrayRepr::Interned { handle, epoch, .. } => WeakJsArray::Interned { handle, epoch },
            JsArrayRepr::Host(ref rc) => WeakJsArray::Host(Rc::downgrade(rc)),
        }
    }
}

impl WeakJsArray {
    pub(crate) fn upgrade(&self) -> Option<JsArrayRef> {
        match self {
            WeakJsArray::Interned { handle, epoch } => {
                crate::page_arena::upgrade_array(*handle, *epoch)
            }
            WeakJsArray::Host(weak) => weak.upgrade().map(JsArrayRef::from_host),
        }
    }

    pub(crate) fn upgrade_value(&self) -> Option<Value> {
        self.upgrade().map(|array| array.as_value())
    }
}

impl Deref for JsArrayRef {
    type Target = RefCell<ArrayStorage>;

    fn deref(&self) -> &Self::Target {
        self.cell()
    }
}

/// View of a [`Value`] with the historical variant names. Used internally
/// so match sites can stay exhaustive without NaN-box tag tests.
pub(crate) enum ValueUnpack {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(JsArrayRef),
    Object(JsObjectRef),
    Function(JsFunction),
}

impl Value {
    #[inline]
    pub(crate) fn unpack(&self) -> ValueUnpack {
        match self {
            Value::Imm(imm) if imm.is_undefined() => ValueUnpack::Undefined,
            Value::Imm(imm) if imm.is_null() => ValueUnpack::Null,
            Value::Imm(imm) if imm.is_bool() => ValueUnpack::Bool(imm.as_bool().unwrap_or(false)),
            Value::Imm(imm) if imm.is_interned_string() => {
                ValueUnpack::String(imm.as_js_string().expect("interned string handle"))
            }
            Value::Imm(imm) if imm.is_object_handle() => ValueUnpack::Object(
                crate::page_arena::get_object(imm.object_handle().expect("object handle")),
            ),
            Value::Imm(imm) if imm.is_array_handle() => ValueUnpack::Array(
                crate::page_arena::get_array(imm.array_handle().expect("array handle")),
            ),
            Value::Imm(imm) if imm.is_function_handle() => ValueUnpack::Function(
                crate::page_arena::get_function(imm.function_handle().expect("function handle")),
            ),
            Value::Imm(imm) => ValueUnpack::Number(imm.as_number().unwrap_or(f64::NAN)),
            Value::String(s) => ValueUnpack::String(s.clone()),
            Value::Array(a) => ValueUnpack::Array(JsArrayRef::from_host(Rc::clone(a))),
            Value::Object(o) => ValueUnpack::Object(JsObjectRef::from_host(Rc::clone(o))),
            Value::Function(f) => ValueUnpack::Function(JsFunction::from_host(Rc::clone(f))),
        }
    }
}

thread_local! {
    /// Internal ECMAScript empty-slot marker. It is identity-based and never
    /// exposed through property reads or iteration.
    static ARRAY_HOLE: Value =
        Value::Object(Rc::new(RefCell::new(crate::JsObject::new())));
}

pub(crate) fn array_hole() -> Value {
    ARRAY_HOLE.with(Clone::clone)
}

pub(crate) fn is_array_hole(value: &Value) -> bool {
    ARRAY_HOLE.with(|hole| match (value, hole) {
        (Value::Object(value), Value::Object(hole)) => Rc::ptr_eq(value, hole),
        _ => false,
    })
}

pub(crate) fn array_slot_value(value: Value) -> Value {
    if is_array_hole(&value) {
        Value::Undefined
    } else {
        value
    }
}

fn function_heap_bytes(props: &HashMap<JsString, Value>) -> usize {
    std::mem::size_of::<FunctionData>()
        .saturating_add(
            props
                .capacity()
                .saturating_mul(std::mem::size_of::<(JsString, Value)>()),
        )
        .saturating_add(props.len().saturating_mul(std::mem::size_of::<JsString>()))
}

pub struct ValueIterator {
    inner: Box<dyn Iterator<Item = Value>>,
}

struct LiveArrayIterator {
    values: JsArrayRef,
    index: usize,
}

struct ProtocolValueIterator {
    iterator: Value,
    done: bool,
}

impl Iterator for ProtocolValueIterator {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = self.iterator.call_method("__w3cos_iterator_next", vec![]);
        if result.get_property("done").to_bool() {
            self.done = true;
            None
        } else {
            Some(result.get_property("value"))
        }
    }
}

impl Iterator for LiveArrayIterator {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.borrow().get(self.index).cloned()?;
        self.index += 1;
        Some(array_slot_value(value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.values.borrow().len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ValueIterator {
    pub(crate) fn new(iterator: impl Iterator<Item = Value> + 'static) -> Self {
        Self {
            inner: Box::new(iterator),
        }
    }

    fn boxed(iterator: Box<dyn Iterator<Item = Value>>) -> Self {
        Self { inner: iterator }
    }

    /// Number of currently remaining values. Live collection iterators may
    /// grow after this observation when their backing Array/Map/Set is mutated.
    pub fn len(&self) -> usize {
        let (lower, upper) = self.inner.size_hint();
        upper.filter(|upper| *upper == lower).unwrap_or(lower)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Iterator for ValueIterator {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        if let (Some(left), Some(right)) = (self.as_js_string(), other.as_js_string()) {
            return left == right;
        }
        match (self, other) {
            (Value::Imm(left), Value::Imm(right)) => left == right,
            (left, right) if Value::array_identity_eq(left, right) => true,
            (left, right) if Value::object_identity_eq(left, right) => true,
            (left, right) if Value::function_identity_eq(left, right) => true,
            _ => false,
        }
    }
}

/// Payload of a JS function object.
///
/// Page-local interned functions store one [`Box`] of this in the page
/// arena. Host / DOM callables share [`Rc<FunctionData>`] on
/// [`Value::Function`] and object call-slots.
pub struct FunctionData {
    inner: Box<dyn Fn(Value, Vec<Value>) -> Value>,
    props: RefCell<HashMap<JsString, Value>>,
    allocation: HeapAllocation,
}

/// A callable JS function: interned page-local handle, or host `Rc`.
///
/// Interned `Clone` copies a `u32` handle (plus a cached payload pointer).
/// It does not `Rc::clone` the closure / props / allocation. Host clones
/// clone one `Rc<FunctionData>`.
pub struct JsFunction {
    repr: JsFunctionRepr,
}

enum JsFunctionRepr {
    Interned {
        handle: u32,
        epoch: u32,
        ptr: *const FunctionData,
        _thread: PhantomData<Rc<()>>,
    },
    Host(Rc<FunctionData>),
}

pub(crate) enum WeakJsFunction {
    Interned { handle: u32, epoch: u32 },
    Host(Weak<FunctionData>),
}

impl FunctionData {
    #[inline]
    pub fn new(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        Self::from_erased(Box::new(f))
    }

    /// Construct the common JS function object after erasing the concrete
    /// Rust closure type. AOT bundles create thousands of distinct closures;
    /// keeping prototype/allocation setup in the generic constructor causes
    /// that body to be monomorphized once per closure before LTO can merge it.
    #[inline(never)]
    fn from_erased(inner: Box<dyn Fn(Value, Vec<Value>) -> Value>) -> Self {
        let mut props = HashMap::new();
        // Ordinary JavaScript function objects own a prototype object. The
        // compiler uses these Values for function declarations/constructors;
        // Libraries install methods on constructor prototypes before
        // constructing instances.
        props.insert(JsString::intern("prototype"), Value::object(HashMap::new()));
        let allocation = HeapAllocation::new(HeapKind::Function, function_heap_bytes(&props));
        Self {
            inner,
            props: RefCell::new(props),
            allocation,
        }
    }

    pub fn call(&self, this: Value, args: Vec<Value>) -> Value {
        (self.inner)(this, args)
    }

    pub fn get_property(&self, key: &str) -> Value {
        self.props
            .borrow()
            .get(key)
            .cloned()
            .unwrap_or(Value::Undefined)
    }

    pub fn has_own_property(&self, key: &str) -> bool {
        self.props.borrow().contains_key(key)
    }

    pub fn keys(&self) -> Vec<String> {
        self.props
            .borrow()
            .keys()
            .map(|key| key.as_str().to_string())
            .collect()
    }

    pub fn set_property(&self, key: &str, value: Value) {
        let mut props = self.props.borrow_mut();
        props.insert(JsString::intern(key), value);
        self.allocation.set_bytes(function_heap_bytes(&props));
    }

    pub fn delete_property(&self, key: &str) {
        let mut props = self.props.borrow_mut();
        props.remove(key);
        self.allocation.set_bytes(function_heap_bytes(&props));
    }

    pub fn identity(&self) -> usize {
        self as *const FunctionData as usize
    }
}

impl Clone for JsFunction {
    fn clone(&self) -> Self {
        match self.repr {
            JsFunctionRepr::Interned {
                handle,
                epoch,
                ptr,
                _thread,
            } => Self {
                repr: JsFunctionRepr::Interned {
                    handle,
                    epoch,
                    ptr,
                    _thread,
                },
            },
            JsFunctionRepr::Host(ref rc) => Self {
                repr: JsFunctionRepr::Host(Rc::clone(rc)),
            },
        }
    }
}

impl JsFunction {
    #[inline]
    pub fn new(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        Self::from_host(Rc::new(FunctionData::new(f)))
    }

    pub(crate) fn from_interned(handle: u32, epoch: u32, ptr: *const FunctionData) -> Self {
        Self {
            repr: JsFunctionRepr::Interned {
                handle,
                epoch,
                ptr,
                _thread: PhantomData,
            },
        }
    }

    pub(crate) fn from_host(data: Rc<FunctionData>) -> Self {
        Self {
            repr: JsFunctionRepr::Host(data),
        }
    }

    /// Resolve payload without holding the page-arena `RefCell`. Interned
    /// handles use the cached slot pointer (epoch-checked). Nested
    /// `Value::function` allocation would otherwise `BorrowMutError`.
    fn data(&self) -> &FunctionData {
        match self.repr {
            JsFunctionRepr::Interned { epoch, ptr, .. } => {
                if epoch != crate::page_arena::current_epoch() || ptr.is_null() {
                    panic!("page function handle used after reset_bridge");
                }
                // Safety: `ptr` names a `FunctionData` box in the current
                // page function table. `reset` bumps the epoch before
                // dropping those boxes. Callers must not hold the reference
                // across a page reset.
                unsafe { &*ptr }
            }
            JsFunctionRepr::Host(ref rc) => rc,
        }
    }

    pub fn call(&self, this: Value, args: Vec<Value>) -> Value {
        self.data().call(this, args)
    }

    /// Read a property of the function object (Undefined when absent).
    pub fn get_property(&self, key: &str) -> Value {
        self.data().get_property(key)
    }

    pub fn has_own_property(&self, key: &str) -> bool {
        self.data().has_own_property(key)
    }

    /// Own property names installed on this function object.
    pub fn keys(&self) -> Vec<String> {
        self.data().keys()
    }

    /// Assign a property on the function object.
    pub fn set_property(&self, key: &str, value: Value) {
        self.data().set_property(key, value)
    }

    pub fn delete_property(&self, key: &str) {
        self.data().delete_property(key)
    }

    /// A stable identity address for this function value (clones of the same
    /// `JsFunction` share it) — used for identity-keyed collections (JS Map).
    pub fn identity(&self) -> usize {
        self.data().identity()
    }

    /// Function identity: two interned handles name the same slot, or two
    /// host functions share the same `FunctionData` allocation.
    pub fn ptr_eq(&self, other: &JsFunction) -> bool {
        match (&self.repr, &other.repr) {
            (
                JsFunctionRepr::Interned {
                    handle: a,
                    epoch: ea,
                    ..
                },
                JsFunctionRepr::Interned {
                    handle: b,
                    epoch: eb,
                    ..
                },
            ) => a == b && ea == eb,
            (JsFunctionRepr::Host(a), JsFunctionRepr::Host(b)) => Rc::ptr_eq(a, b),
            _ => std::ptr::eq(self.data(), other.data()),
        }
    }

    #[cfg(test)]
    pub(crate) fn interned_handle(&self) -> Option<u32> {
        match self.repr {
            JsFunctionRepr::Interned { handle, .. } => Some(handle),
            JsFunctionRepr::Host(_) => None,
        }
    }

    /// Strong count of the host `Rc<FunctionData>`, when this is not interned.
    #[cfg(test)]
    pub(crate) fn host_strong_count(&self) -> Option<usize> {
        match &self.repr {
            JsFunctionRepr::Host(rc) => Some(Rc::strong_count(rc)),
            JsFunctionRepr::Interned { .. } => None,
        }
    }

    pub(crate) fn downgrade(&self) -> WeakJsFunction {
        match self.repr {
            JsFunctionRepr::Interned { handle, epoch, .. } => {
                WeakJsFunction::Interned { handle, epoch }
            }
            JsFunctionRepr::Host(ref rc) => WeakJsFunction::Host(Rc::downgrade(rc)),
        }
    }
}

impl WeakJsFunction {
    pub(crate) fn upgrade_value(&self) -> Option<Value> {
        match self {
            WeakJsFunction::Interned { handle, epoch } => {
                crate::page_arena::upgrade_function(*handle, *epoch)
                    .map(|_| Value::Imm(Immediate::from_function_handle(*handle)))
            }
            WeakJsFunction::Host(weak) => weak.upgrade().map(Value::Function),
        }
    }
}

impl fmt::Debug for JsFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Function]")
    }
}

impl fmt::Debug for FunctionData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Function]")
    }
}

// ── Type coercion ──────────────────────────────────────────────────────

impl Value {
    /// Stable identity hash with ECMAScript `Object.is` semantics.
    ///
    /// Heap values use reference identity, while primitives use their value.
    /// This is suitable for framework hook dependency comparison.
    pub fn identity_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        if let Some(value) = crate::bigint::get(self) {
            8_u8.hash(&mut hasher);
            value.to_string().hash(&mut hasher);
            return hasher.finish();
        }
        match self.unpack() {
            ValueUnpack::Undefined => 0_u8.hash(&mut hasher),
            ValueUnpack::Null => 1_u8.hash(&mut hasher),
            ValueUnpack::Bool(value) => {
                2_u8.hash(&mut hasher);
                value.hash(&mut hasher);
            }
            ValueUnpack::Number(value) => {
                3_u8.hash(&mut hasher);
                if value.is_nan() {
                    u64::MAX.hash(&mut hasher);
                } else {
                    value.to_bits().hash(&mut hasher);
                }
            }
            ValueUnpack::String(value) => {
                4_u8.hash(&mut hasher);
                value.hash(&mut hasher);
            }
            ValueUnpack::Array(value) => {
                5_u8.hash(&mut hasher);
                value.identity().hash(&mut hasher);
            }
            ValueUnpack::Object(value) => {
                6_u8.hash(&mut hasher);
                value.identity().hash(&mut hasher);
            }
            ValueUnpack::Function(value) => {
                7_u8.hash(&mut hasher);
                value.identity().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// ECMAScript `typeof` operator.
    pub fn type_of(&self) -> &'static str {
        if crate::bigint::get(self).is_some() {
            return "bigint";
        }
        match self.unpack() {
            ValueUnpack::Undefined => "undefined",
            ValueUnpack::Null => "object",
            ValueUnpack::Bool(_) => "boolean",
            ValueUnpack::Number(_) => "number",
            ValueUnpack::String(value) if value.starts_with("__w3cos_symbol_") => "symbol",
            ValueUnpack::String(_) => "string",
            ValueUnpack::Array(_) | ValueUnpack::Object(_) => "object",
            ValueUnpack::Function(_) => "function",
        }
    }

    /// ECMAScript `ToBoolean`.
    pub fn to_bool(&self) -> bool {
        if let Some(zero) = crate::bigint::is_zero(self) {
            return !zero;
        }
        match self.unpack() {
            ValueUnpack::Undefined | ValueUnpack::Null => false,
            ValueUnpack::Bool(b) => b,
            ValueUnpack::Number(n) => n != 0.0 && !n.is_nan(),
            ValueUnpack::String(s) => !s.is_empty(),
            ValueUnpack::Array(_) | ValueUnpack::Object(_) | ValueUnpack::Function(_) => true,
        }
    }

    /// ECMAScript `ToNumber`.
    pub fn to_number(&self) -> f64 {
        if let Some(value) = crate::bigint::get(self) {
            return value.to_string().parse().unwrap_or_else(|_| {
                if value.sign() == num_bigint::Sign::Minus {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            });
        }
        match self.unpack() {
            ValueUnpack::Undefined => f64::NAN,
            ValueUnpack::Null => 0.0,
            ValueUnpack::Bool(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
            ValueUnpack::Number(n) => n,
            ValueUnpack::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }

    /// ECMAScript `ToString`.
    pub fn to_js_string(&self) -> String {
        if let Some(value) = crate::bigint::get(self) {
            return value.to_string();
        }
        match self.unpack() {
            ValueUnpack::Undefined => "undefined".into(),
            ValueUnpack::Null => "null".into(),
            ValueUnpack::Bool(b) => b.to_string(),
            ValueUnpack::Number(n) => {
                if n.is_nan() {
                    "NaN".into()
                } else if n.is_infinite() {
                    if n > 0.0 {
                        "Infinity".into()
                    } else {
                        "-Infinity".into()
                    }
                } else if n == 0.0 {
                    "0".into()
                } else if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                    format!("{}", n as i64)
                } else {
                    format!("{}", n)
                }
            }
            ValueUnpack::String(s) => s.as_str().to_string(),
            ValueUnpack::Array(arr) => {
                let elems: Vec<String> = arr
                    .borrow()
                    .iter()
                    .map(|value| {
                        if is_array_hole(value) || value.is_nullish() {
                            String::new()
                        } else {
                            value.to_js_string()
                        }
                    })
                    .collect();
                elems.join(",")
            }
            ValueUnpack::Object(_) => {
                let to_string = self.get_property("toString");
                if to_string.is_function() {
                    if let Some(value) = to_string.call(self.clone(), vec![]).as_js_string() {
                        return value.as_str().to_string();
                    }
                }
                "[object Object]".into()
            }
            ValueUnpack::Function(function) => {
                let to_string = function.get_property("toString");
                if to_string.is_function() || to_string.is_object() || to_string.is_callable() {
                    if let Some(value) = to_string.call(self.clone(), vec![]).as_js_string() {
                        return value.as_str().to_string();
                    }
                }
                "function() { [native code] }".into()
            }
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Imm(imm) if imm.is_undefined())
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Imm(imm) if imm.is_null())
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Imm(imm) if imm.is_undefined() || imm.is_null())
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Imm(imm) if imm.is_number())
    }
    pub fn is_string(&self) -> bool {
        match self {
            Value::String(_) => true,
            Value::Imm(imm) => imm.is_interned_string(),
            _ => false,
        }
    }
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Imm(imm) if imm.is_bool())
    }
    pub fn is_object(&self) -> bool {
        self.as_object().is_some() && crate::bigint::get(self).is_none()
    }
    pub fn is_array(&self) -> bool {
        self.as_array().is_some()
    }
    pub fn is_function(&self) -> bool {
        match self {
            Value::Function(_) => true,
            Value::Imm(imm) => imm.is_function_handle(),
            _ => false,
        }
    }
    pub fn is_callable(&self) -> bool {
        match self {
            Value::Function(_) => true,
            Value::Imm(imm) if imm.is_function_handle() => true,
            _ => self
                .as_object()
                .is_some_and(|object| object.borrow().call_slot().is_some()),
        }
    }

    /// ECMAScript `ToInt32`.
    pub fn to_i32(&self) -> i32 {
        let n = self.to_number();
        if n.is_nan() || n.is_infinite() || n == 0.0 {
            return 0;
        }
        let i = n.trunc() as i64;
        (i % (1i64 << 32)) as i32
    }

    /// ECMAScript `ToUint32`.
    pub fn to_u32(&self) -> u32 {
        self.to_i32() as u32
    }

    /// ECMAScript `in` operator: `key in obj`.
    pub fn js_in(&self, obj: &Value) -> Value {
        let key = self.to_js_string();
        if let Some(o) = obj.as_object() {
            return Value::Bool(o.borrow().has(&key));
        }
        if let Some(arr) = obj.as_array() {
            return if let Ok(idx) = key.parse::<usize>() {
                Value::Bool(
                    arr.borrow()
                        .get(idx)
                        .is_some_and(|value| !is_array_hole(value)),
                )
            } else {
                Value::Bool(false)
            };
        }
        Value::Bool(false)
    }

    /// Property access: `obj[key]` or `obj.key`.
    pub fn get_property(&self, key: &str) -> Value {
        if let Some(o) = self.as_object() {
            let value = o.borrow().get(key, self).clone();
            return if !value.is_undefined() || !o.borrow().may_have_getter_properties() {
                value
            } else {
                let getter = o
                    .borrow()
                    .get(&format!("__w3cos_getter_{key}"), self)
                    .clone();
                getter.call(self.clone(), vec![])
            };
        }
        if let Some(arr) = self.as_array() {
            if let Some(value) = crate::binary::typed_array_property(self, key) {
                return value;
            }
            return if let Ok(idx) = key.parse::<usize>() {
                arr.borrow()
                    .get(idx)
                    .cloned()
                    .map(array_slot_value)
                    .unwrap_or(Value::Undefined)
            } else if key == "length" {
                Value::Number(arr.borrow().len() as f64)
            } else {
                Value::Undefined
            };
        }
        match self {
            _ if self.is_string() => {
                let s = self.as_js_string().expect("string");
                if let Ok(idx) = key.parse::<usize>() {
                    s.encode_utf16()
                        .nth(idx)
                        .and_then(|unit| String::from_utf16(&[unit]).ok())
                        .map(Value::from)
                        .unwrap_or(Value::Undefined)
                } else if key == "length" {
                    Value::Number(s.encode_utf16().count() as f64)
                } else {
                    Value::Undefined
                }
            }
            // JS functions are objects: read attached properties.
            _ if self.as_function().is_some() => {
                self.as_function().expect("function").get_property(key)
            }
            _ => Value::Undefined,
        }
    }

    /// Property access for a JavaScript member expression.
    ///
    /// Host/runtime code can use [`Value::get_property`] as a total lookup,
    /// while compiled `value.key` / `value[key]` expressions must reject a
    /// nullish receiver per ECMAScript `GetValue` semantics. Optional chaining
    /// performs its nullish guard before calling the total lookup.
    pub fn get_property_checked(&self, key: &str) -> Value {
        if self.is_nullish() {
            let receiver = if self.is_null() { "null" } else { "undefined" };
            crate::throw_value(crate::js_object! {
                "name" => "TypeError",
                "message" => format!("Cannot read properties of {receiver} (reading '{key}')"),
            });
        }
        self.get_property(key)
    }

    /// ECMAScript object-rest copy used by `{ picked, ...rest }`.
    ///
    /// Only own enumerable string properties are copied; the prototype and
    /// excluded bindings are not carried into the result.
    pub fn object_rest(&self, excluded: &[&str]) -> Value {
        let Some(object) = self.as_object() else {
            return Value::object(HashMap::new());
        };
        let object = object.borrow();
        let properties = object
            .keys()
            .into_iter()
            .filter(|key| !excluded.contains(&key.as_str()))
            .map(|key| {
                let value = object.get_direct(&key);
                (key, value)
            })
            .collect();
        Value::object(properties)
    }

    /// Property assignment: `obj[key] = value`.
    ///
    /// Mirrors the `__w3cos_getter_` read convention with a setter one: when
    /// the object has no own data property `key` but a `__w3cos_setter_{key}`
    /// function is reachable through the prototype chain, the setter is
    /// invoked with the object as receiver instead of storing directly.
    pub fn set_property(&self, key: &str, value: Value) {
        if let Some(o) = self.as_object() {
            let has_own = o.borrow().properties.contains_key(key);
            if !has_own {
                let setter = o
                    .borrow()
                    .get(&format!("__w3cos_setter_{key}"), self)
                    .clone();
                if !setter.is_undefined() {
                    setter.call(self.clone(), vec![value]);
                    return;
                }
            }
            o.borrow_mut().set(key, value, &Value::Undefined);
            return;
        }
        if let Some(arr) = self.as_array() {
            if let Ok(idx) = key.parse::<usize>() {
                if crate::binary::set_typed_array_index(self, idx, value.clone()) {
                    return;
                }
                let mut a = arr.borrow_mut();
                if idx >= a.len() {
                    a.resize_with(idx + 1, array_hole);
                }
                a[idx] = value;
            }
            return;
        }
        match self {
            // JS functions are objects: properties attach to the function
            // value (decorator ids, constructor statics).
            _ if self.as_function().is_some() => self
                .as_function()
                .expect("function")
                .set_property(key, value),
            _ => {}
        }
    }

    /// Delete an own property and return the JavaScript-style success value.
    pub fn delete_property(&self, key: &str) -> Value {
        let deleted = match self {
            _ if self.as_object().is_some() => {
                self.as_object().expect("object").borrow_mut().delete(key)
            }
            _ if self.as_array().is_some() => {
                let array = self.as_array().expect("array");
                if let Ok(index) = key.parse::<usize>() {
                    if let Some(slot) = array.borrow_mut().get_mut(index) {
                        *slot = array_hole();
                    }
                }
                true
            }
            _ if self.as_function().is_some() => {
                self.as_function().expect("function").delete_property(key);
                true
            }
            _ => true,
        };
        Value::Bool(deleted)
    }
}

// ── Constructors ───────────────────────────────────────────────────────

impl Value {
    pub fn from_f64(n: f64) -> Self {
        Value::Number(n)
    }
    pub fn from_bool(b: bool) -> Self {
        Value::Bool(b)
    }
    pub fn string(s: &str) -> Self {
        Value::from_js_string(JsString::intern(s))
    }

    pub fn array(items: Vec<Value>) -> Self {
        let handle = crate::page_arena::alloc_array(ArrayStorage::new(items));
        Value::Imm(Immediate::from_array_handle(handle))
    }

    pub fn object(props: HashMap<String, Value>) -> Self {
        let handle = crate::page_arena::alloc_object(crate::JsObject::from_map(props));
        Value::Imm(Immediate::from_object_handle(handle))
    }

    pub fn object_from_parts(parts: Vec<Value>) -> Self {
        let mut properties = HashMap::new();
        for part in parts {
            if let Some(object) = part.as_object() {
                let object = object.borrow();
                for key in object.keys() {
                    properties.insert(key.clone(), object.get_direct(&key));
                }
            }
        }
        Value::object(properties)
    }

    pub fn function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        let handle = crate::page_arena::alloc_function(FunctionData::new(f));
        Value::Imm(Immediate::from_function_handle(handle))
    }

    /// A plain object that is also callable (a JS class / constructor object).
    pub fn callable(
        props: HashMap<String, Value>,
        f: impl Fn(Value, Vec<Value>) -> Value + 'static,
    ) -> Self {
        Value::Object(Rc::new(RefCell::new(crate::JsObject::with_call_slot(
            props,
            Rc::new(FunctionData::new(f)),
        ))))
    }

    /// Invoke a dynamically lowered JavaScript function value.
    pub fn call(&self, this: Value, args: Vec<Value>) -> Value {
        match self {
            Value::Function(function) => function.call(this, args),
            _ if self.as_function().is_some() => {
                self.as_function().expect("function").call(this, args)
            }
            _ if self.as_object().is_some() => {
                let object = self.as_object().expect("object");
                let slot = object.borrow().call_slot().cloned();
                match slot {
                    Some(function) => function.call(this, args),
                    None => Value::Undefined,
                }
            }
            _ => Value::Undefined,
        }
    }

    /// Invoke a property as a method while preserving the JavaScript receiver.
    pub fn call_method(&self, key: &str, args: Vec<Value>) -> Value {
        if key == "__w3cos_symbol_iterator" {
            if let Some(iterator) = acquire_custom_iterator(self, "__w3cos_symbol_iterator", args) {
                return iterator;
            }
            return iterator_object(self.iter());
        }
        if key == "__w3cos_symbol_async_iterator" {
            if let Some(iterator) =
                acquire_custom_iterator(self, "__w3cos_symbol_async_iterator", args.clone())
            {
                return iterator;
            }
            // Host objects historically published the camelCase alias used by
            // `Symbol.asyncIterator` in older ESM lowering.
            if let Some(iterator) =
                acquire_custom_iterator(self, "__w3cos_symbol_asyncIterator", args.clone())
            {
                return iterator;
            }
            return self.call_method("__w3cos_symbol_iterator", args);
        }
        if key == "__w3cos_iterator_next" {
            let next = self.get_property("next");
            if !next.is_callable() {
                throw_value(type_error("iterator next method is not callable"));
            }
            let result = next.call(self.clone(), args);
            if !is_iterator_object(&result) {
                throw_value(type_error("iterator next method must return an object"));
            }
            return result;
        }
        if key == "__w3cos_async_iterator_next" {
            let next = self.get_property("next");
            if !next.is_callable() {
                throw_value(type_error("iterator next method is not callable"));
            }
            let result = next.call(self.clone(), args);
            return crate::promise::resolve(vec![result]).call_method(
                "then",
                vec![Value::function(|_, args| {
                    let result = args.first().cloned().unwrap_or(Value::Undefined);
                    if !is_iterator_object(&result) {
                        throw_value(type_error("iterator next method must return an object"));
                    }
                    result
                })],
            );
        }
        if key == "__w3cos_iterator_close_return" {
            if let Some(exception) = close_iterator_chain(self, None) {
                throw_value(exception);
            }
            return Value::Undefined;
        }
        if key == "__w3cos_iterator_close_throw" {
            let pending = args.first().cloned().unwrap_or(Value::Undefined);
            return close_iterator_chain(self, Some(pending.clone())).unwrap_or(pending);
        }
        if key == "__w3cos_async_iterator_close_return" {
            return close_async_iterator_chain(self, None, false);
        }
        if key == "__w3cos_async_iterator_close_throw" {
            let pending = args.first().cloned().unwrap_or(Value::Undefined);
            return close_async_iterator_chain(self, Some(pending), true);
        }
        if key == "__w3cos_for_in_keys" {
            return crate::intrinsics::for_in_keys(self);
        }
        if key == "__w3cos_super_ctor" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            return crate::class::super_ctor(&receiver, self, args.into_iter().skip(1).collect());
        }
        if key == "__w3cos_super_method" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            return crate::class::super_method(
                &receiver,
                self,
                &name,
                args.into_iter().skip(2).collect(),
            );
        }
        if key == "__w3cos_super_get" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            return crate::class::super_get(&receiver, self, &name);
        }
        if key == "__w3cos_super_set" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            let value = args.get(2).cloned().unwrap_or(Value::Undefined);
            return crate::class::super_set(&receiver, self, &name, value);
        }
        if key == "__w3cos_static_super_method" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            return crate::class::static_super_method(
                &receiver,
                self,
                &name,
                args.into_iter().skip(2).collect(),
            );
        }
        if key == "__w3cos_static_super_get" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            return crate::class::static_super_get(&receiver, self, &name);
        }
        if key == "__w3cos_static_super_set" {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            let value = args.get(2).cloned().unwrap_or(Value::Undefined);
            return crate::class::static_super_set(&receiver, self, &name, value);
        }
        if key == "hasOwnProperty" {
            let property = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            return Value::Bool(match self {
                _ if self.as_object().is_some() => self
                    .as_object()
                    .expect("object")
                    .borrow()
                    .properties
                    .contains_key(property.as_str()),
                _ if self.as_array().is_some() => {
                    let values = self.as_array().expect("array");
                    property == "length"
                        || property.parse::<usize>().is_ok_and(|index| {
                            values
                                .borrow()
                                .get(index)
                                .is_some_and(|value| !is_array_hole(value))
                        })
                }
                _ if self.as_function().is_some() => self
                    .as_function()
                    .expect("function")
                    .has_own_property(&property),
                _ => false,
            });
        }
        if crate::binary::is_typed_array(self)
            && let Some(result) = crate::binary::typed_array_call_method(self, key, args.clone())
        {
            return result;
        }
        if let Some(values) = self.as_array()
            && let Some(result) = array_call_method(&values, key, args.clone(), self)
        {
            return result;
        }
        match (self, key) {
            (this, "call") if this.is_function() => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                return self.call(this_arg, args.into_iter().skip(1).collect());
            }
            (this, "apply") if this.is_function() => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let applied_args = match args.get(1).and_then(Value::as_array) {
                    Some(values) => values
                        .borrow()
                        .iter()
                        .cloned()
                        .map(array_slot_value)
                        .collect(),
                    None => Vec::new(),
                };
                return self.call(this_arg, applied_args);
            }
            (this, "bind") if this.is_function() => {
                let target = self.clone();
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let bound_args: Vec<Value> = args.into_iter().skip(1).collect();
                return Value::function(move |_, call_args| {
                    let mut combined = bound_args.clone();
                    combined.extend(call_args);
                    target.call(this_arg.clone(), combined)
                });
            }
            (this, "endsWith") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                return Value::Bool(
                    args.first()
                        .is_some_and(|suffix| value.ends_with(&suffix.to_js_string())),
                );
            }
            (this, "slice") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let units = value.encode_utf16().collect::<Vec<_>>();
                let length = units.len() as i64;
                let normalize = |argument: Option<&Value>, fallback: i64| {
                    let raw = argument
                        .map(Value::to_number)
                        .filter(|number| number.is_finite())
                        .map(|number| number.trunc() as i64)
                        .unwrap_or(fallback);
                    if raw < 0 {
                        (length + raw).max(0) as usize
                    } else {
                        raw.min(length) as usize
                    }
                };
                let start = normalize(args.first(), 0);
                let end = normalize(args.get(1), length).max(start);
                return Value::from(String::from_utf16_lossy(&units[start..end]));
            }
            (this, "substr") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let units = value.encode_utf16().collect::<Vec<_>>();
                let length = units.len() as i64;
                let raw_start = args
                    .first()
                    .map(Value::to_number)
                    .filter(|number| number.is_finite())
                    .map(|number| number.trunc() as i64)
                    .unwrap_or(0);
                let start = if raw_start < 0 {
                    (length + raw_start).max(0)
                } else {
                    raw_start.min(length)
                } as usize;
                let count = args
                    .get(1)
                    .map(Value::to_number)
                    .filter(|number| number.is_finite())
                    .map(|number| number.trunc().max(0.0) as usize)
                    .unwrap_or(units.len() - start);
                let end = start.saturating_add(count).min(units.len());
                return Value::from(String::from_utf16_lossy(&units[start..end]));
            }
            (this, "startsWith") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let needle = args.first().cloned().unwrap_or_default().to_js_string();
                let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let start = string_index_to_byte(&value, start);
                return Value::Bool(value[start..].starts_with(&needle));
            }
            (this, "includes") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let needle = args.first().cloned().unwrap_or_default().to_js_string();
                let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let start = string_index_to_byte(&value, start);
                return Value::Bool(value[start..].contains(&needle));
            }
            (this, "indexOf") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let needle = args.first().cloned().unwrap_or_default().to_js_string();
                let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let start_byte = string_index_to_byte(&value, start);
                let index = value
                    .get(start_byte..)
                    .and_then(|tail| tail.find(&needle).map(|offset| start_byte + offset))
                    .map(|byte| value[..byte].chars().count() as f64)
                    .unwrap_or(-1.0);
                return Value::Number(index);
            }
            (this, "charCodeAt") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let index = args.first().map(Value::to_number).unwrap_or(0.0);
                if !index.is_finite() || index < 0.0 {
                    return Value::Number(f64::NAN);
                }
                return Value::Number(
                    value
                        .encode_utf16()
                        .nth(index as usize)
                        .map(f64::from)
                        .unwrap_or(f64::NAN),
                );
            }
            (this, "charAt") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let index = args.first().map(Value::to_number).unwrap_or(0.0);
                if !index.is_finite() || index < 0.0 {
                    return Value::from(String::new());
                }
                return Value::from(
                    value
                        .chars()
                        .nth(index as usize)
                        .map(|character| character.to_string())
                        .unwrap_or_default(),
                );
            }
            (this, "substring") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let len = value.chars().count();
                let mut start = args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let mut end = args
                    .get(1)
                    .map(Value::to_number)
                    .unwrap_or(len as f64)
                    .max(0.0) as usize;
                start = start.min(len);
                end = end.min(len);
                if start > end {
                    std::mem::swap(&mut start, &mut end);
                }
                let start = string_index_to_byte(&value, start);
                let end = string_index_to_byte(&value, end);
                return Value::from(value[start..end].to_string());
            }
            (this, "toUpperCase") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                return Value::from(value.to_uppercase());
            }
            (this, "toLowerCase") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                return Value::from(value.to_lowercase());
            }
            (this, "localeCompare") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let other = args.first().cloned().unwrap_or_default().to_js_string();
                return Value::Number(match value.as_str().cmp(other.as_str()) {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                });
            }
            (this, "trim") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                return Value::from(value.trim().to_string());
            }
            (this, "split") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let Some(separator) = args.first() else {
                    return Value::array(vec![Value::from(value.clone())]);
                };
                if separator.is_undefined() {
                    return Value::array(vec![Value::from(value.clone())]);
                }
                let limit = args
                    .get(1)
                    .map(|value| value.to_number().max(0.0) as usize)
                    .unwrap_or(usize::MAX);
                if let Some(result) = crate::regexp::string_split(&value, separator, limit) {
                    return result;
                }
                let separator = separator.to_js_string();
                let parts: Vec<Value> = if separator.is_empty() {
                    value
                        .chars()
                        .take(limit)
                        .map(|ch| Value::from(ch.to_string()))
                        .collect()
                } else {
                    value
                        .split(&separator)
                        .take(limit)
                        .map(|part| Value::from(part.to_string()))
                        .collect()
                };
                return Value::array(parts);
            }
            (this, "match") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let pattern = args.first().cloned().unwrap_or(Value::Undefined);
                if let Some(result) = crate::regexp::string_match(&value, &pattern) {
                    return result;
                }
            }
            (this, "matchAll") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let pattern = args.first().cloned().unwrap_or(Value::Undefined);
                if let Some(result) = crate::regexp::string_match_all(&value, &pattern) {
                    return result;
                }
                let global = crate::regexp::create(&pattern.to_js_string(), "g");
                return crate::regexp::string_match_all(&value, &global)
                    .expect("fresh global regexp always produces an iterator");
            }
            (this, "search") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let pattern = args.first().cloned().unwrap_or(Value::Undefined);
                if let Some(result) = crate::regexp::string_search(&value, &pattern) {
                    return result;
                }
                return Value::Number(
                    value
                        .find(&pattern.to_js_string())
                        .map(|byte| value[..byte].encode_utf16().count() as f64)
                        .unwrap_or(-1.0),
                );
            }
            (this, "replace") if this.is_string() => {
                let value = this.as_js_string().expect("string");
                let pattern = args.first().cloned().unwrap_or(Value::Undefined);
                let replacement = args.get(1).cloned().unwrap_or(Value::Undefined);
                if let Some(result) = crate::regexp::string_replace(&value, &pattern, &replacement)
                {
                    return result;
                }
                if replacement.is_function()
                    && let Some(byte) = value.find(&pattern.to_js_string())
                {
                    let matched = pattern.to_js_string();
                    let result = replacement.call(
                        Value::Undefined,
                        vec![
                            pattern,
                            Value::Number(value[..byte].encode_utf16().count() as f64),
                            Value::from(value.clone()),
                        ],
                    );
                    return Value::from(format!(
                        "{}{}{}",
                        &value[..byte],
                        result.to_js_string(),
                        &value[byte + matched.len()..]
                    ));
                }
                return Value::from(value.replacen(
                    &pattern.to_js_string(),
                    &replacement.to_js_string(),
                    1,
                ));
            }
            _ if self.as_array().is_some() && key == "filter" => {
                let values = self.as_array().expect("array");
                let predicate = args.first().cloned().unwrap_or(Value::Undefined);
                let filtered = values
                    .borrow()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, value)| {
                        if is_array_hole(value) {
                            return None;
                        }
                        predicate
                            .call(
                                Value::Undefined,
                                vec![value.clone(), Value::Number(index as f64)],
                            )
                            .to_bool()
                            .then(|| value.clone())
                    })
                    .collect();
                return Value::array(filtered);
            }
            _ if self.as_array().is_some() && key == "push" => {
                let values = self.as_array().expect("array");
                let mut values = values.borrow_mut();
                values.extend(args);
                return Value::Number(values.len() as f64);
            }
            _ if self.as_array().is_some() && key == "set" => {
                let values = self.as_array().expect("array");
                let source: Vec<Value> = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .iter()
                    .collect();
                let offset = array_index(args.get(1), values.borrow().len(), 0);
                if crate::binary::is_typed_array(self) {
                    for (index, value) in source.into_iter().enumerate() {
                        self.set_property(&(offset + index).to_string(), value);
                    }
                    return Value::Undefined;
                }
                let mut target = values.borrow_mut();
                for (index, value) in source.into_iter().enumerate() {
                    if let Some(slot) = target.get_mut(offset + index) {
                        *slot = value;
                    }
                }
                return Value::Undefined;
            }
            _ if self.as_array().is_some() && key == "forEach" => {
                let values = self.as_array().expect("array");
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                for (index, value) in values.borrow().iter().cloned().enumerate() {
                    if is_array_hole(&value) {
                        continue;
                    }
                    callback.call(
                        Value::Undefined,
                        vec![value, Value::Number(index as f64), self.clone()],
                    );
                }
                return Value::Undefined;
            }
            _ => {}
        }
        self.get_property(key).call(self.clone(), args)
    }

    pub fn is_iterable(&self) -> bool {
        if !self.get_property("__w3cos_symbol_iterator").is_undefined() {
            return true;
        }
        if crate::binary::typed_array_value_iterator(self).is_some() {
            return true;
        }
        match self {
            _ if self.is_array() => true,
            _ if self.is_string() => true,
            _ if self.as_object().is_some() => {
                let object = self.as_object().expect("object");
                crate::collections::iter_collection(self).is_some()
                    || object
                        .borrow()
                        .get_direct("__w3cosIterableSnapshot")
                        .is_function()
                    || self.get_property("_first").is_object()
                    || object
                        .borrow()
                        .get_direct("__w3cosMapValuesSnapshot")
                        .is_function()
                    || object.borrow().get_direct("__w3cosMapValues").is_array()
            }
            _ => false,
        }
    }

    pub fn iter(&self) -> ValueIterator {
        if let Some(iterator) = acquire_custom_iterator(self, "__w3cos_symbol_iterator", vec![]) {
            return ValueIterator::new(ProtocolValueIterator {
                iterator,
                done: false,
            });
        }
        if let Some(iterator) = crate::binary::typed_array_value_iterator(self) {
            return iterator;
        }
        match self {
            _ if self.as_array().is_some() => ValueIterator::new(LiveArrayIterator {
                values: self.as_array().expect("array"),
                index: 0,
            }),
            _ if self.is_string() => {
                let value = self.as_js_string().expect("string");
                ValueIterator::new(
                    value
                        .chars()
                        .map(|character| Value::from(character.to_string()))
                        .collect::<Vec<_>>()
                        .into_iter(),
                )
            }
            // First use the standards-oriented Map/Set registry. Retain the
            // host runtime's snapshot hook as a fallback for its lightweight
            // built-in Map used by compiled application paths.
            _ if self.as_object().is_some() => {
                let object = self.as_object().expect("object");
                if let Some(iterator) = crate::collections::iter_collection(self) {
                    return ValueIterator::boxed(iterator);
                }
                let iterable_snapshot = object.borrow().get_direct("__w3cosIterableSnapshot");
                if iterable_snapshot.is_function() {
                    if let Some(values) = iterable_snapshot.call(self.clone(), vec![]).as_array() {
                        return ValueIterator::new(values.borrow().clone().into_iter());
                    }
                }
                // Monaco's command registry stores commands in its own
                // LinkedList implementation. Generator lowering is still a
                // best-effort path, so expose that conventional `_first` /
                // `next` node chain through the runtime iterator bridge.
                let first = self.get_property("_first");
                if first.is_object() {
                    let mut values = Vec::new();
                    let mut node = first;
                    while node.is_object() {
                        let element = node.get_property("element");
                        if element.is_undefined() {
                            break;
                        }
                        values.push(element);
                        let next = node.get_property("next");
                        if next.strict_eq(&node) {
                            break;
                        }
                        node = next;
                    }
                    return ValueIterator::new(values.into_iter());
                }
                // The AOT lowering turns common `Map#forEach(value => ...)`
                // loops into this iterator path. Map exposes a live values
                // snapshot so the lowered loop retains JavaScript semantics.
                let snapshot = object.borrow().get_direct("__w3cosMapValuesSnapshot");
                let values = if snapshot.is_function() {
                    snapshot.call(self.clone(), vec![])
                } else {
                    object.borrow().get_direct("__w3cosMapValues")
                };
                match values.as_array() {
                    Some(values) => ValueIterator::new(values.borrow().clone().into_iter()),
                    None => ValueIterator::new(Vec::new().into_iter()),
                }
            }
            _ => ValueIterator::new(Vec::new().into_iter()),
        }
    }
}

pub(crate) fn iterator_object(iterator: ValueIterator) -> Value {
    let iterator = Rc::new(RefCell::new(iterator));
    let next_iterator = Rc::clone(&iterator);
    let next = Value::function(move |_, _| {
        if let Some(value) = next_iterator.borrow_mut().next() {
            crate::js_object! {
                "value" => value,
                "done" => Value::Bool(false),
            }
        } else {
            crate::js_object! {
                "value" => Value::Undefined,
                "done" => Value::Bool(true),
            }
        }
    });
    let object = crate::js_object! { "next" => next };
    let iterator = object.clone();
    object.set_property(
        "__w3cos_symbol_iterator",
        Value::function(move |_, _| iterator.clone()),
    );
    object
}

fn acquire_custom_iterator(value: &Value, key: &str, args: Vec<Value>) -> Option<Value> {
    let method = value.get_property(key);
    if method.is_undefined() {
        return None;
    }
    if !method.is_callable() {
        throw_value(type_error("value's iterator method is not callable"));
    }
    let iterator = method.call(value.clone(), args);
    if !is_iterator_object(&iterator) {
        throw_value(type_error("iterator method must return an object"));
    }
    Some(iterator)
}

fn close_async_iterator_chain(
    iterators: &Value,
    pending_throw: Option<Value>,
    return_pending: bool,
) -> Value {
    let completion = Rc::new(RefCell::new(pending_throw));
    let mut chain = crate::promise::resolve(vec![Value::Undefined]);
    for iterator in iterators.iter() {
        let completion = Rc::clone(&completion);
        chain = chain.call_method(
            "then",
            vec![Value::function(move |_, _| {
                let return_method = iterator.get_property("return");
                if return_method.is_undefined() {
                    return Value::Undefined;
                }
                if !return_method.is_callable() {
                    *completion.borrow_mut() =
                        Some(type_error("iterator return method is not callable"));
                    return Value::Undefined;
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    return_method.call(iterator.clone(), Vec::new())
                }));
                let result = match outcome {
                    Ok(result) => result,
                    Err(payload) => {
                        if completion.borrow().is_none() {
                            *completion.borrow_mut() =
                                Some(crate::promise::payload_to_value(payload));
                        }
                        return Value::Undefined;
                    }
                };
                let fulfilled_completion = Rc::clone(&completion);
                let on_fulfilled = Value::function(move |_, args| {
                    let result = args.first().cloned().unwrap_or(Value::Undefined);
                    if fulfilled_completion.borrow().is_none() && !is_iterator_object(&result) {
                        *fulfilled_completion.borrow_mut() =
                            Some(type_error("iterator return method must return an object"));
                    }
                    Value::Undefined
                });
                let rejected_completion = Rc::clone(&completion);
                let on_rejected = Value::function(move |_, args| {
                    if rejected_completion.borrow().is_none() {
                        *rejected_completion.borrow_mut() =
                            Some(args.first().cloned().unwrap_or(Value::Undefined));
                    }
                    Value::Undefined
                });
                crate::promise::resolve(vec![result])
                    .call_method("then", vec![on_fulfilled, on_rejected])
            })],
        );
    }
    let completion = Rc::clone(&completion);
    chain.call_method(
        "then",
        vec![Value::function(move |_, _| {
            match completion.borrow().clone() {
                Some(value) if return_pending => value,
                Some(value) => throw_value(value),
                None => Value::Undefined,
            }
        })],
    )
}

fn is_iterator_object(value: &Value) -> bool {
    (value.is_object() || value.is_array() || value.is_function())
        && crate::bigint::get(value).is_none()
}

pub(crate) fn type_error(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ]))
}

fn close_iterator_chain(iterators: &Value, mut throw_completion: Option<Value>) -> Option<Value> {
    for iterator in iterators.iter() {
        let return_method = iterator.get_property("return");
        if return_method.is_undefined() {
            continue;
        }
        if !return_method.is_callable() {
            // GetMethod failures precede the completion-priority check in
            // IteratorClose, so they replace even an existing throw.
            throw_completion = Some(type_error("iterator return method is not callable"));
            continue;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            return_method.call(iterator.clone(), Vec::new())
        }));
        match outcome {
            Ok(result) => {
                if throw_completion.is_none() && !is_iterator_object(&result) {
                    throw_completion =
                        Some(type_error("iterator return method must return an object"));
                }
            }
            Err(payload) => match payload.downcast::<PanicValue>() {
                Ok(exception) => {
                    if throw_completion.is_none() {
                        throw_completion = Some(exception.0);
                    }
                }
                Err(payload) => std::panic::resume_unwind(payload),
            },
        }
    }
    throw_completion
}

/// Normalize a JS array index argument (`undefined` → `default`; negatives
/// wrap from the end; clamped to `len`).
fn array_index(value: Option<&Value>, len: usize, default: usize) -> usize {
    let Some(value) = value else {
        return default.min(len);
    };
    if value.is_undefined() {
        return default.min(len);
    }
    let n = value.to_number();
    if n.is_nan() {
        0
    } else if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        (n as usize).min(len)
    }
}

fn string_index_to_byte(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

/// The JS `Array.prototype` method set for [`Value::Array`]. Returns `None`
/// for names the dedicated match arms in [`Value::call_method`] implement
/// (`filter`/`push`/`forEach`) or don't cover at all.
fn array_call_method(
    values: &JsArrayRef,
    key: &str,
    args: Vec<Value>,
    this: &Value,
) -> Option<Value> {
    let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Undefined);
    let callback_args = |value: &Value, index: usize| {
        vec![value.clone(), Value::Number(index as f64), this.clone()]
    };
    Some(match key {
        "filter" | "push" | "forEach" | "set" => return None, // handled by dedicated arms
        "pop" => values
            .borrow_mut()
            .pop()
            .map(array_slot_value)
            .unwrap_or(Value::Undefined),
        "shift" => {
            if values.borrow().is_empty() {
                Value::Undefined
            } else {
                array_slot_value(values.borrow_mut().remove(0))
            }
        }
        "unshift" => {
            let mut values = values.borrow_mut();
            values.values.splice(0..0, args.iter().cloned());
            values.refresh_heap_accounting();
            Value::Number(values.len() as f64)
        }
        "slice" => {
            let values = values.borrow();
            let start = array_index(args.first(), values.len(), 0);
            let end = array_index(args.get(1), values.len(), values.len());
            Value::array(values[start.min(end)..end].to_vec())
        }
        "splice" => {
            let mut values = values.borrow_mut();
            let start = array_index(args.first(), values.len(), 0);
            let delete_count = match args.get(1) {
                None => values.len() - start,
                Some(v) if v.is_undefined() => values.len() - start,
                Some(v) => (v.to_number().max(0.0) as usize).min(values.len() - start),
            };
            let mut tail = values.split_off(start);
            let removed: Vec<Value> = tail.drain(..delete_count.min(tail.len())).collect();
            for (offset, item) in args.iter().skip(2).enumerate() {
                tail.insert(offset, item.clone());
            }
            values.extend(tail);
            Value::array(removed)
        }
        "map" => {
            let f = arg(0);
            let mapped = values
                .borrow()
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    if is_array_hole(value) {
                        array_hole()
                    } else {
                        f.call(Value::Undefined, callback_args(value, index))
                    }
                })
                .collect();
            Value::array(mapped)
        }
        "find" => {
            let f = arg(0);
            values
                .borrow()
                .iter()
                .enumerate()
                .find(|(index, value)| {
                    let value = array_slot_value((*value).clone());
                    f.call(Value::Undefined, callback_args(&value, *index))
                        .to_bool()
                })
                .map(|(_, value)| array_slot_value(value.clone()))
                .unwrap_or(Value::Undefined)
        }
        "findIndex" => {
            let f = arg(0);
            let index = values
                .borrow()
                .iter()
                .enumerate()
                .find(|(index, value)| {
                    let value = array_slot_value((*value).clone());
                    f.call(Value::Undefined, callback_args(&value, *index))
                        .to_bool()
                })
                .map(|(index, _)| index as f64)
                .unwrap_or(-1.0);
            Value::Number(index)
        }
        "some" => {
            let f = arg(0);
            let hit = values
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, value)| !is_array_hole(value))
                .any(|(index, value)| {
                    f.call(Value::Undefined, callback_args(value, index))
                        .to_bool()
                });
            Value::Bool(hit)
        }
        "every" => {
            let f = arg(0);
            let all = values
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, value)| !is_array_hole(value))
                .all(|(index, value)| {
                    f.call(Value::Undefined, callback_args(value, index))
                        .to_bool()
                });
            Value::Bool(all)
        }
        "includes" => {
            let needle = arg(0);
            let hit = values
                .borrow()
                .iter()
                .cloned()
                .map(array_slot_value)
                .any(|value| value.strict_eq(&needle));
            Value::Bool(hit)
        }
        "indexOf" => {
            let needle = arg(0);
            let index = values
                .borrow()
                .iter()
                .position(|value| !is_array_hole(value) && value.strict_eq(&needle))
                .map(|index| index as f64)
                .unwrap_or(-1.0);
            Value::Number(index)
        }
        "lastIndexOf" => {
            let needle = arg(0);
            let index = values
                .borrow()
                .iter()
                .rposition(|value| !is_array_hole(value) && value.strict_eq(&needle))
                .map(|index| index as f64)
                .unwrap_or(-1.0);
            Value::Number(index)
        }
        "join" => {
            let separator = match args.first() {
                None => ",".to_string(),
                Some(v) if v.is_undefined() => ",".to_string(),
                Some(v) => v.to_js_string(),
            };
            Value::from(
                values
                    .borrow()
                    .iter()
                    .map(|value| {
                        if is_array_hole(value) || value.is_nullish() {
                            String::new()
                        } else {
                            value.to_js_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(&separator),
            )
        }
        "concat" => {
            let mut out = values.borrow().clone();
            for item in &args {
                if let Some(inner) = item.as_array() {
                    out.extend(inner.borrow().iter().cloned());
                } else {
                    out.push(item.clone());
                }
            }
            Value::array(out)
        }
        "reduce" => {
            let f = arg(0);
            let values = values.borrow();
            let (mut acc, start) = match args.get(1) {
                Some(init) => (init.clone(), 0),
                None => match values
                    .iter()
                    .enumerate()
                    .find(|(_, value)| !is_array_hole(value))
                {
                    Some((index, first)) => (first.clone(), index + 1),
                    None => return Some(Value::Undefined),
                },
            };
            for (index, value) in values.iter().enumerate().skip(start) {
                if is_array_hole(value) {
                    continue;
                }
                acc = f.call(
                    Value::Undefined,
                    vec![
                        acc,
                        value.clone(),
                        Value::Number(index as f64),
                        this.clone(),
                    ],
                );
            }
            acc
        }
        "reduceRight" => {
            let f = arg(0);
            let values = values.borrow();
            let (mut acc, start) = match args.get(1) {
                Some(init) => (init.clone(), values.len()),
                None => match values
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, value)| !is_array_hole(value))
                {
                    Some((index, last)) => (last.clone(), index),
                    None => return Some(Value::Undefined),
                },
            };
            for index in (0..start).rev() {
                if is_array_hole(&values[index]) {
                    continue;
                }
                acc = f.call(
                    Value::Undefined,
                    vec![
                        acc,
                        values[index].clone(),
                        Value::Number(index as f64),
                        this.clone(),
                    ],
                );
            }
            acc
        }
        "sort" => {
            let comparator = args.first().cloned();
            let mut sorted = values.borrow().clone();
            sorted.sort_by(|left, right| match &comparator {
                _ if is_array_hole(left) && is_array_hole(right) => std::cmp::Ordering::Equal,
                _ if is_array_hole(left) => std::cmp::Ordering::Greater,
                _ if is_array_hole(right) => std::cmp::Ordering::Less,
                Some(f) if !f.is_undefined() => {
                    let order = f
                        .call(Value::Undefined, vec![left.clone(), right.clone()])
                        .to_number();
                    order.total_cmp(&0.0)
                }
                _ => left.to_js_string().cmp(&right.to_js_string()),
            });
            let mut values = values.borrow_mut();
            values.values = sorted;
            values.refresh_heap_accounting();
            this.clone()
        }
        "reverse" => {
            values.borrow_mut().reverse();
            this.clone()
        }
        "flat" => {
            let depth = args
                .first()
                .map(|v| v.to_number().max(0.0) as usize)
                .unwrap_or(1);
            fn flatten(into: &mut Vec<Value>, items: &[Value], depth: usize) {
                for item in items {
                    if is_array_hole(item) {
                        continue;
                    }
                    if depth > 0 {
                        if let Some(inner) = item.as_array() {
                            flatten(into, &inner.borrow(), depth - 1);
                            continue;
                        }
                    }
                    into.push(item.clone());
                }
            }
            let mut out = Vec::new();
            flatten(&mut out, &values.borrow(), depth);
            Value::array(out)
        }
        "flatMap" => {
            let f = arg(0);
            let mut out = Vec::new();
            for (index, value) in values.borrow().iter().enumerate() {
                if is_array_hole(value) {
                    continue;
                }
                let mapped = f.call(Value::Undefined, callback_args(value, index));
                if let Some(inner) = mapped.as_array() {
                    out.extend(inner.borrow().iter().cloned());
                } else {
                    out.push(mapped);
                }
            }
            Value::array(out)
        }
        "at" => {
            let values = values.borrow();
            let index = array_index(args.first(), values.len(), values.len());
            values
                .get(index)
                .cloned()
                .map(array_slot_value)
                .unwrap_or(Value::Undefined)
        }
        _ => return None,
    })
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::from_js_string(JsString::intern(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::from_js_string(JsString::from(value))
    }
}

impl From<JsString> for Value {
    fn from(value: JsString) -> Self {
        Value::from_js_string(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Number(value)
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Value::Number(value as f64)
    }
}

macro_rules! impl_number_value_from {
    ($($ty:ty),* $(,)?) => {
        $(
            impl From<$ty> for Value {
                fn from(value: $ty) -> Self {
                    Value::Number(value as f64)
                }
            }
        )*
    };
}

impl_number_value_from!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Value::array(value)
    }
}

#[macro_export]
macro_rules! js_object {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut properties = ::std::collections::HashMap::new();
        $(properties.insert(($key).to_string(), $crate::Value::from($value));)*
        $crate::Value::object(properties)
    }};
}

// ── Arithmetic / comparison operators ──────────────────────────────────

impl Value {
    /// ECMAScript `+` (addition or string concatenation).
    pub fn js_add(&self, other: &Value) -> Value {
        if self.is_string() || other.is_string() {
            Value::from(format!("{}{}", self.to_js_string(), other.to_js_string()))
        } else if let Some(value) = crate::bigint::add(self, other) {
            value
        } else {
            Value::Number(self.to_number() + other.to_number())
        }
    }

    pub fn js_sub(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::sub(self, other) {
            return value;
        }
        Value::Number(self.to_number() - other.to_number())
    }

    pub fn js_mul(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::mul(self, other) {
            return value;
        }
        Value::Number(self.to_number() * other.to_number())
    }

    pub fn js_div(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::div(self, other) {
            return value;
        }
        Value::Number(self.to_number() / other.to_number())
    }

    pub fn js_rem(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::rem(self, other) {
            return value;
        }
        Value::Number(self.to_number() % other.to_number())
    }

    pub fn js_neg(&self) -> Value {
        if let Some(value) = crate::bigint::neg(self) {
            return value;
        }
        Value::Number(-self.to_number())
    }

    pub fn js_pow(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::pow(self, other) {
            return value;
        }
        Value::Number(self.to_number().powf(other.to_number()))
    }

    // ── Bitwise operators ──

    pub fn js_bitor(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::bitor(self, other) {
            return value;
        }
        Value::Number((self.to_i32() | other.to_i32()) as f64)
    }

    pub fn js_bitand(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::bitand(self, other) {
            return value;
        }
        Value::Number((self.to_i32() & other.to_i32()) as f64)
    }

    pub fn js_bitxor(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::bitxor(self, other) {
            return value;
        }
        Value::Number((self.to_i32() ^ other.to_i32()) as f64)
    }

    pub fn js_bitnot(&self) -> Value {
        if let Some(value) = crate::bigint::bitnot(self) {
            return value;
        }
        Value::Number((!self.to_i32()) as f64)
    }

    pub fn js_shl(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::shift_left(self, other) {
            return value;
        }
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number((self.to_i32() << shift) as f64)
    }

    pub fn js_shr(&self, other: &Value) -> Value {
        if let Some(value) = crate::bigint::shift_right(self, other) {
            return value;
        }
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number((self.to_i32() >> shift) as f64)
    }

    pub fn js_ushr(&self, other: &Value) -> Value {
        if crate::bigint::get(self).is_some() || crate::bigint::get(other).is_some() {
            crate::throw_value(crate::Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("BigInts have no unsigned right shift"),
                ),
            ])));
        }
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number(((self.to_i32() as u32) >> shift) as f64)
    }

    /// ECMAScript `===` (strict equality).
    pub fn strict_eq(&self, other: &Value) -> bool {
        if let Some(equal) = crate::bigint::equals(self, other) {
            return equal;
        }
        match (self, other) {
            (Value::Imm(a), Value::Imm(b)) => {
                match (a.as_number(), b.as_number()) {
                    (Some(x), Some(y)) => x == y, // NaN !== NaN
                    (None, None) => a == b,
                    _ => false,
                }
            }
            (left, right) if Value::array_identity_eq(left, right) => true,
            (left, right) if Value::object_identity_eq(left, right) => true,
            (left, right) if Value::function_identity_eq(left, right) => true,
            _ => match (self.as_js_string(), other.as_js_string()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        }
    }

    /// ECMAScript SameValueZero (Map/Set key equality): strict equality for
    /// primitives except NaN equals NaN and -0 equals +0; Array/Object keys
    /// compare by reference identity (`Rc` pointer), Function keys by shared
    /// closure identity (clones of one function value are the same key).
    pub fn same_value_zero(&self, other: &Value) -> bool {
        if let Some(equal) = crate::bigint::equals(self, other) {
            return equal;
        }
        match (self, other) {
            (Value::Imm(a), Value::Imm(b)) => match (a.as_number(), b.as_number()) {
                (Some(x), Some(y)) => x == y || (x.is_nan() && y.is_nan()),
                (None, None) => a == b,
                _ => false,
            },
            (left, right) if Value::array_identity_eq(left, right) => true,
            (left, right) if Value::object_identity_eq(left, right) => true,
            (left, right) if Value::function_identity_eq(left, right) => true,
            _ => match (self.as_js_string(), other.as_js_string()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        }
    }

    /// ECMAScript `==` (abstract equality — simplified).
    pub fn abstract_eq(&self, other: &Value) -> bool {
        match (self.unpack(), other.unpack()) {
            (ValueUnpack::Undefined, ValueUnpack::Undefined)
            | (ValueUnpack::Null, ValueUnpack::Null)
            | (ValueUnpack::Bool(_), ValueUnpack::Bool(_))
            | (ValueUnpack::Number(_), ValueUnpack::Number(_))
            | (ValueUnpack::String(_), ValueUnpack::String(_))
            | (ValueUnpack::Array(_), ValueUnpack::Array(_))
            | (ValueUnpack::Object(_), ValueUnpack::Object(_))
            | (ValueUnpack::Function(_), ValueUnpack::Function(_)) => self.strict_eq(other),
            (ValueUnpack::Null, ValueUnpack::Undefined)
            | (ValueUnpack::Undefined, ValueUnpack::Null) => true,
            (ValueUnpack::Number(_), ValueUnpack::String(_)) => {
                self.strict_eq(&Value::Number(other.to_number()))
            }
            (ValueUnpack::String(_), ValueUnpack::Number(_)) => {
                Value::Number(self.to_number()).strict_eq(other)
            }
            (ValueUnpack::Bool(_), _) => Value::Number(self.to_number()).abstract_eq(other),
            (_, ValueUnpack::Bool(_)) => self.abstract_eq(&Value::Number(other.to_number())),
            _ => false,
        }
    }

    pub fn js_lt(&self, other: &Value) -> bool {
        if let Some(ordering) = crate::bigint::compare(self, other) {
            return ordering.is_lt();
        }
        match (self, other) {
            (left, right) if left.is_string() && right.is_string() => {
                left.as_js_string() < right.as_js_string()
            }
            _ => self.to_number() < other.to_number(),
        }
    }

    pub fn js_gt(&self, other: &Value) -> bool {
        if let Some(ordering) = crate::bigint::compare(self, other) {
            return ordering.is_gt();
        }
        match (self, other) {
            (left, right) if left.is_string() && right.is_string() => {
                left.as_js_string() > right.as_js_string()
            }
            _ => self.to_number() > other.to_number(),
        }
    }

    pub fn js_le(&self, other: &Value) -> bool {
        if let Some(ordering) = crate::bigint::compare(self, other) {
            return !ordering.is_gt();
        }
        match (self, other) {
            (left, right) if left.is_string() && right.is_string() => {
                left.as_js_string() <= right.as_js_string()
            }
            _ => self.to_number() <= other.to_number(),
        }
    }

    pub fn js_ge(&self, other: &Value) -> bool {
        if let Some(ordering) = crate::bigint::compare(self, other) {
            return !ordering.is_lt();
        }
        match (self, other) {
            (left, right) if left.is_string() && right.is_string() => {
                left.as_js_string() >= right.as_js_string()
            }
            _ => self.to_number() >= other.to_number(),
        }
    }

    pub fn js_not(&self) -> Value {
        Value::Bool(!self.to_bool())
    }
}

// ── Display ────────────────────────────────────────────────────────────

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_js_string())
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.unpack() {
            ValueUnpack::Undefined => write!(f, "undefined"),
            ValueUnpack::Null => write!(f, "null"),
            ValueUnpack::Bool(b) => write!(f, "{b}"),
            ValueUnpack::Number(n) => write!(f, "{n}"),
            ValueUnpack::String(s) => write!(f, "{s:?}"),
            ValueUnpack::Array(arr) => write!(f, "{:?}", arr.borrow()),
            ValueUnpack::Object(_) => write!(f, "{{...}}"),
            ValueUnpack::Function(_) => write!(f, "[Function]"),
        }
    }
}

/// Standalone `type_of` function matching the generated code's `type_of(expr)` calls.
pub fn type_of(val: &Value) -> Value {
    Value::string(val.type_of())
}

/// An Error-shaped object (`{ message }`) for runtime failures. Compiled JS
/// raises exceptions via `std::panic::panic_any`; builtins that need to
/// signal a JS exception (invalid JSON, circular structures, bad URLs)
/// build one of these and [`throw_value`] it so compiled `try/catch` sees
/// a JS-style error value.
pub(crate) fn js_error(message: &str) -> Value {
    let mut properties = HashMap::new();
    properties.insert("message".to_string(), Value::string(message));
    Value::object(properties)
}

/// Panic payload for JS exceptions.
///
/// `std::panic::panic_any` requires a `Send` payload and `Value` is not
/// `Send` (it holds `Rc`s), so JS `throw` cannot panic with a bare
/// `Value`. This newtype wraps it; the `Send` impl is sound here because
/// the runtime is single-threaded — the payload only ever travels from a
/// `throw_value` call site to a `catch_unwind` on the same thread.
pub struct PanicValue(pub Value);

// SAFETY: w3cos values are single-threaded by design (Rc/RefCell
// everywhere); the wrapper never crosses an actual thread boundary.
unsafe impl Send for PanicValue {}

/// Raise a JS exception: `throw value` in compiled code and in builtins.
/// Unwinds until a `catch_unwind` (compiled `try/catch`, or the promise
/// reaction runner, which turns it into a rejection).
pub fn throw_value(value: Value) -> ! {
    // Debug channel: W3COS_JS_CONSOLE=1 prints thrown values (incl. Error
    // objects with message/stack props) before unwinding — without it an
    // uncaught JS throw only shows Rust's opaque "Box<dyn Any>".
    if std::env::var_os("W3COS_JS_CONSOLE").is_some() {
        let message = value.get_property("message");
        let diagnostics = value.get_property("diagnostics");
        let detail = if !diagnostics.is_undefined() {
            crate::json::stringify(vec![diagnostics]).to_js_string()
        } else if message.is_undefined() {
            value.to_js_string()
        } else {
            message.to_js_string()
        };
        eprintln!("[js.throw] {detail}");
    }
    std::panic::panic_any(PanicValue(value))
}

/// JS completion record used by AOT state machines: `Ok` is normal
/// completion, `Err` is throw. Host/DOM seams may still panic via
/// [`throw_value`].
pub type Completion = Result<Value, Value>;

fn panic_payload_to_throw(payload: Box<dyn std::any::Any + Send>) -> Value {
    if let Some(value) = payload.downcast_ref::<PanicValue>() {
        value.0.clone()
    } else {
        std::panic::resume_unwind(payload)
    }
}

/// Catch a [`throw_value`] panic as a Throw completion. Non-JS panics resume.
pub fn catch_js<F: FnOnce() -> Value>(f: F) -> Completion {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => Ok(value),
        Err(payload) => Err(panic_payload_to_throw(payload)),
    }
}

/// Like [`catch_js`], but the closure may itself return a completion.
pub fn catch_js_result<T, F: FnOnce() -> Result<T, Value>>(f: F) -> Result<T, Value> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => Err(panic_payload_to_throw(payload)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_copy<T: Copy>(value: T) -> T {
        value
    }

    /// Immediates are a Copy tagged word: clone is a register move, no `Rc`,
    /// no allocation. Heap strings/arrays/objects/functions stay as pointers.
    #[test]
    fn immediates_are_copy_tagged_words_without_allocation() {
        assert_eq!(std::mem::size_of::<Immediate>(), 8);
        assert!(!std::mem::needs_drop::<Immediate>());
        let undef = assert_copy(Immediate::UNDEFINED);
        let null = assert_copy(Immediate::NULL);
        let flag = assert_copy(Immediate::from_bool(true));
        let num = assert_copy(Immediate::from_number(42.0));
        let nan = assert_copy(Immediate::from_number(f64::NAN));
        assert!(undef.is_undefined());
        assert!(null.is_null());
        assert_eq!(flag.as_bool(), Some(true));
        assert_eq!(num.as_number(), Some(42.0));
        assert!(nan.as_number().unwrap().is_nan());
        // Payload NaNs collapse to the canonical quiet-NaN so tags stay unique.
        assert_eq!(
            nan.bits(),
            Immediate::from_number(f64::from_bits(0x7FF8_0000_0000_0001)).bits()
        );

        let a = Value::Undefined;
        let b = a.clone();
        assert!(b.is_undefined());
        // Clone of an immediate Value does not allocate: the Imm arm is Copy.
        match a {
            Value::Imm(word) => {
                let copied = assert_copy(word);
                assert!(copied.is_undefined());
            }
            _ => panic!("undefined must be the tagged immediate word"),
        }
        match Value::Bool(false) {
            Value::Imm(word) => assert_eq!(assert_copy(word).as_bool(), Some(false)),
            _ => panic!("bool must be the tagged immediate word"),
        }
        match Value::Number(-0.0) {
            Value::Imm(word) => assert_eq!(
                assert_copy(word).as_number().unwrap().to_bits(),
                (-0.0_f64).to_bits()
            ),
            _ => panic!("number must be the tagged immediate word"),
        }
        // Direct String-arm construction stays a heap variant; interned
        // short strings created via Value::string are packed below.
        assert!(
            Value::String(JsString::intern("heap-or-intern"))
                .as_immediate()
                .is_none()
        );
        match Value::string("imm-intern") {
            Value::Imm(word) => {
                let copied = assert_copy(word);
                assert!(copied.is_interned_string());
                assert_eq!(std::mem::size_of_val(&copied), 8);
                assert!(!std::mem::needs_drop::<Immediate>());
            }
            _ => panic!("interned short string must be the tagged immediate word"),
        }
        match Value::array(vec![]) {
            Value::Imm(word) => {
                assert!(word.is_array_handle());
                assert!(!std::mem::needs_drop::<Immediate>());
            }
            _ => panic!("Value::array must pack a page-arena handle"),
        }
        match Value::object(HashMap::new()) {
            Value::Imm(word) => {
                assert!(word.is_object_handle());
                assert!(!std::mem::needs_drop::<Immediate>());
            }
            _ => panic!("Value::object must pack a page-arena handle"),
        }
        match Value::function(|_, _| Value::Undefined) {
            Value::Imm(word) => {
                assert!(word.is_function_handle());
                assert!(!std::mem::needs_drop::<Immediate>());
            }
            _ => panic!("Value::function must pack a page-arena handle"),
        }
    }

    #[test]
    fn type_coercion() {
        assert_eq!(Value::Undefined.to_bool(), false);
        assert_eq!(Value::Null.to_bool(), false);
        assert_eq!(Value::Bool(false).to_bool(), false);
        assert_eq!(Value::Number(0.0).to_bool(), false);
        assert_eq!(Value::String("".into()).to_bool(), false);
        assert_eq!(Value::Number(1.0).to_bool(), true);
        assert_eq!(Value::String("x".into()).to_bool(), true);
    }

    #[test]
    fn type_of_values() {
        assert_eq!(Value::Undefined.type_of(), "undefined");
        assert_eq!(Value::Null.type_of(), "object");
        assert_eq!(Value::Number(42.0).type_of(), "number");
        assert_eq!(Value::String("hi".into()).type_of(), "string");
        assert_eq!(Value::Bool(true).type_of(), "boolean");
        assert_eq!(
            Value::String("__w3cos_symbol_for:react.element".into()).type_of(),
            "symbol"
        );
    }

    #[test]
    fn sparse_array_slots_are_absent_but_iterate_as_undefined() {
        let array = Value::array(vec![
            Value::Number(1.0),
            Value::Undefined,
            Value::Number(3.0),
        ]);
        assert_eq!(array.delete_property("1"), Value::Bool(true));

        assert_eq!(array.get_property("length"), Value::Number(3.0));
        assert!(array.get_property("1").is_undefined());
        assert_eq!(Value::string("1").js_in(&array), Value::Bool(false));
        assert_eq!(
            array.call_method("hasOwnProperty", vec![Value::string("1")]),
            Value::Bool(false)
        );
        assert_eq!(
            array.iter().collect::<Vec<_>>(),
            vec![Value::Number(1.0), Value::Undefined, Value::Number(3.0)]
        );
        assert_eq!(array.to_js_string(), "1,,3");
        assert_eq!(
            crate::json::stringify(vec![array.clone()]),
            Value::string("[1,null,3]")
        );
        assert_eq!(crate::builtins::object_keys(&array).to_js_string(), "0,2");
        assert_eq!(crate::intrinsics::for_in_keys(&array).to_js_string(), "0,2");
        let copied = Value::object(HashMap::new());
        crate::intrinsics::copy_data_properties(&copied, &array);
        assert_eq!(
            copied.call_method("hasOwnProperty", vec![Value::string("1")]),
            Value::Bool(false)
        );
        assert_eq!(copied.get_property("2"), Value::Number(3.0));

        let visited = Rc::new(RefCell::new(Vec::new()));
        let callback_visited = Rc::clone(&visited);
        array.call_method(
            "forEach",
            vec![Value::function(move |_, arguments| {
                callback_visited
                    .borrow_mut()
                    .push(arguments[1].to_number() as usize);
                Value::Undefined
            })],
        );
        assert_eq!(visited.borrow().as_slice(), &[0, 2]);

        let mapped = array.call_method(
            "map",
            vec![Value::function(|_, arguments| arguments[0].clone())],
        );
        assert_eq!(Value::string("1").js_in(&mapped), Value::Bool(false));

        array.set_property("5", Value::Number(6.0));
        assert_eq!(array.get_property("length"), Value::Number(6.0));
        assert_eq!(Value::string("4").js_in(&array), Value::Bool(false));
    }

    #[test]
    fn plain_objects_expose_has_own_property() {
        let object = crate::js_object! {
            "present" => Value::Undefined,
        };
        assert!(
            object
                .call_method("hasOwnProperty", vec![Value::from("present")])
                .to_bool()
        );
        assert!(
            !object
                .call_method("hasOwnProperty", vec![Value::from("missing")])
                .to_bool()
        );
    }

    #[test]
    fn arithmetic() {
        let a = Value::Number(10.0);
        let b = Value::Number(3.0);
        assert_eq!(a.js_add(&b).to_number(), 13.0);
        assert_eq!(a.js_sub(&b).to_number(), 7.0);
        assert_eq!(a.js_mul(&b).to_number(), 30.0);
    }

    #[test]
    fn string_concat() {
        let a = Value::String("hello".into());
        let b = Value::Number(42.0);
        assert_eq!(a.js_add(&b).to_js_string(), "hello42");
    }

    #[test]
    fn string_methods_cover_token_parsing() {
        let token = Value::String("source.ts".into());
        assert_eq!(
            token
                .call_method("indexOf", vec![Value::from(".")])
                .to_number(),
            6.0
        );
        assert_eq!(
            token
                .call_method("substring", vec![Value::Number(7.0)])
                .to_js_string(),
            "ts"
        );
        assert_eq!(
            Value::from("a😀c")
                .call_method("substr", vec![Value::Number(1.0), Value::Number(2.0)])
                .to_js_string(),
            "😀"
        );
        assert_eq!(
            token
                .call_method("substr", vec![Value::Number(-2.0)])
                .to_js_string(),
            "ts"
        );
        assert_eq!(
            Value::from("abc")
                .call_method("toUpperCase", vec![])
                .to_js_string(),
            "ABC"
        );
        assert_eq!(
            Value::from("a b")
                .call_method("split", vec![Value::from(" ")])
                .get_property("1")
                .to_js_string(),
            "b"
        );
        assert_eq!(
            Value::from("a\n").call_method("charCodeAt", vec![Value::Number(1.0)]),
            Value::Number(10.0)
        );
        assert!(
            Value::from("a")
                .call_method("charCodeAt", vec![Value::Number(2.0)])
                .to_number()
                .is_nan()
        );
    }

    #[test]
    fn strict_equality() {
        assert!(Value::Number(1.0).strict_eq(&Value::Number(1.0)));
        assert!(!Value::Number(f64::NAN).strict_eq(&Value::Number(f64::NAN)));
        assert!(Value::String("a".into()).strict_eq(&Value::String("a".into())));
        assert!(!Value::Number(1.0).strict_eq(&Value::String("1".into())));

        let array = Value::array(vec![]);
        let other_array = Value::array(vec![]);
        assert!(array.strict_eq(&array.clone()));
        assert!(!array.strict_eq(&other_array));

        let object = Value::object(HashMap::new());
        let other_object = Value::object(HashMap::new());
        assert!(object.strict_eq(&object.clone()));
        assert!(!object.strict_eq(&other_object));

        let function = Value::function(|_, _| Value::Undefined);
        let other_function = Value::function(|_, _| Value::Undefined);
        assert!(function.strict_eq(&function.clone()));
        assert!(!function.strict_eq(&other_function));
    }

    #[test]
    fn identity_hash_tracks_heap_identity_and_object_is_numbers() {
        let function = Value::function(|_, _| Value::Undefined);
        assert_eq!(function.identity_hash(), function.clone().identity_hash());
        assert_ne!(
            function.identity_hash(),
            Value::function(|_, _| Value::Undefined).identity_hash()
        );

        let object = Value::object(HashMap::new());
        assert_eq!(object.identity_hash(), object.clone().identity_hash());
        assert_ne!(
            object.identity_hash(),
            Value::object(HashMap::new()).identity_hash()
        );

        assert_eq!(
            Value::Number(f64::NAN).identity_hash(),
            Value::Number(-f64::NAN).identity_hash()
        );
        assert_ne!(
            Value::Number(0.0).identity_hash(),
            Value::Number(-0.0).identity_hash()
        );
    }

    #[test]
    fn abstract_equality() {
        assert!(Value::Null.abstract_eq(&Value::Undefined));
        assert!(Value::Number(1.0).abstract_eq(&Value::String("1".into())));
        assert!(Value::Bool(true).abstract_eq(&Value::Number(1.0)));
    }

    #[test]
    fn relational_comparison_is_lexicographic_for_two_strings() {
        assert!(Value::from("function").js_lt(&Value::from("u")));
        assert!(Value::from("10").js_lt(&Value::from("2")));
        assert!(Value::from("u").js_ge(&Value::from("u")));
        assert!(!Value::from("z").js_le(&Value::from("a")));
        assert!(Value::from("10").js_gt(&Value::Number(2.0)));
    }

    #[test]
    fn to_js_string() {
        assert_eq!(Value::Undefined.to_js_string(), "undefined");
        assert_eq!(Value::Null.to_js_string(), "null");
        assert_eq!(Value::Number(42.0).to_js_string(), "42");
        assert_eq!(Value::Number(3.14).to_js_string(), "3.14");
        assert_eq!(Value::Bool(true).to_js_string(), "true");

        let named_function = Value::function(|_, _| Value::Undefined);
        named_function.set_property(
            "toString",
            Value::function(|_, _| Value::String("modelService".into())),
        );
        assert_eq!(named_function.to_js_string(), "modelService");

        let plain_function = Value::function(|_, _| Value::Undefined);
        assert_eq!(
            plain_function.to_js_string(),
            "function() { [native code] }"
        );
    }

    #[test]
    fn bitwise_operations() {
        let a = Value::Number(5.0);
        let b = Value::Number(3.0);
        assert_eq!(a.js_bitor(&b).to_number(), 7.0);
        assert_eq!(a.js_bitand(&b).to_number(), 1.0);
        assert_eq!(a.js_bitxor(&b).to_number(), 6.0);
        assert_eq!(a.js_shl(&Value::Number(1.0)).to_number(), 10.0);
        assert_eq!(a.js_shr(&Value::Number(1.0)).to_number(), 2.0);
    }

    #[test]
    fn power_operator() {
        assert_eq!(
            Value::Number(2.0).js_pow(&Value::Number(10.0)).to_number(),
            1024.0
        );
        assert_eq!(
            Value::Number(9.0).js_pow(&Value::Number(0.5)).to_number(),
            3.0
        );
    }

    #[test]
    fn function_call_apply_and_bind_preserve_receiver_and_arguments() {
        let function = Value::function(|this, args| {
            Value::Number(
                this.get_property("base").to_number()
                    + args.iter().map(Value::to_number).sum::<f64>(),
            )
        });
        let receiver = Value::object(HashMap::from([("base".to_string(), Value::Number(10.0))]));

        assert_eq!(
            function
                .call_method(
                    "call",
                    vec![receiver.clone(), Value::Number(2.0), Value::Number(3.0)],
                )
                .to_number(),
            15.0
        );
        assert_eq!(
            function
                .call_method(
                    "apply",
                    vec![
                        receiver.clone(),
                        Value::array(vec![Value::Number(4.0), Value::Number(5.0)]),
                    ],
                )
                .to_number(),
            19.0
        );
        let bound = function.call_method("bind", vec![receiver, Value::Number(6.0)]);
        assert_eq!(
            bound
                .call(Value::Undefined, vec![Value::Number(7.0)])
                .to_number(),
            23.0
        );
    }

    #[test]
    fn to_i32_conversion() {
        assert_eq!(Value::Number(42.7).to_i32(), 42);
        assert_eq!(Value::Number(-3.9).to_i32(), -3);
        assert_eq!(Value::Number(f64::NAN).to_i32(), 0);
        assert_eq!(Value::Number(f64::INFINITY).to_i32(), 0);
    }

    #[test]
    fn in_operator() {
        let mut props = HashMap::new();
        props.insert("name".to_string(), Value::String("test".into()));
        let obj = Value::object(props);
        assert!(Value::String("name".into()).js_in(&obj).to_bool());
        assert!(!Value::String("age".into()).js_in(&obj).to_bool());

        let arr = Value::array(vec![Value::Number(10.0), Value::Number(20.0)]);
        assert!(Value::Number(0.0).js_in(&arr).to_bool());
        assert!(Value::Number(1.0).js_in(&arr).to_bool());
        assert!(!Value::Number(2.0).js_in(&arr).to_bool());
    }

    #[test]
    fn js_object_macro_builds_dynamic_properties() {
        let object = crate::js_object! {
            "rowCount" => 1_000,
            "label" => "rows",
            "enabled" => true,
        };
        assert_eq!(object.get_property("rowCount").to_number(), 1_000.0);
        assert_eq!(object.get_property("label").to_js_string(), "rows");
        assert!(object.get_property("enabled").to_bool());
    }

    #[test]
    fn dynamic_function_call_preserves_receiver() {
        let receiver = crate::js_object! { "value" => 42 };
        receiver.set_property(
            "read",
            Value::function(|this, _| this.get_property("value")),
        );
        assert_eq!(receiver.call_method("read", vec![]).to_number(), 42.0);
    }

    #[test]
    fn symbol_iterator_walks_linked_list_style_objects() {
        let sentinel = crate::js_object! { "element" => Value::Undefined };
        let second = crate::js_object! {
            "element" => "second",
            "next" => sentinel,
        };
        let first = crate::js_object! {
            "element" => "first",
            "next" => second,
        };
        let list = crate::js_object! { "_first" => first };

        let iterator = list.call_method("__w3cos_symbol_iterator", vec![]);
        let first_result = iterator.call_method("next", vec![]);
        let second_result = iterator.call_method("next", vec![]);
        let done_result = iterator.call_method("next", vec![]);

        assert_eq!(first_result.get_property("value").to_js_string(), "first");
        assert!(!first_result.get_property("done").to_bool());
        assert_eq!(second_result.get_property("value").to_js_string(), "second");
        assert!(done_result.get_property("done").to_bool());
    }

    #[test]
    fn aot_iter_uses_the_same_custom_iterator_protocol_as_dynamic_calls() {
        let index = Rc::new(RefCell::new(0usize));
        let next_index = Rc::clone(&index);
        let iterator = crate::js_object! {
            "next" => Value::function(move |_, _| {
                let mut index = next_index.borrow_mut();
                if *index < 2 {
                    *index += 1;
                    crate::js_object! {
                        "value" => Value::Number(*index as f64),
                        "done" => Value::Bool(false),
                    }
                } else {
                    crate::js_object! {
                        "value" => Value::Undefined,
                        "done" => Value::Bool(true),
                    }
                }
            }),
        };
        let custom_iterator = iterator.clone();
        let iterable = crate::js_object! {
            "__w3cos_symbol_iterator" => Value::function(move |_, _| {
                custom_iterator.clone()
            }),
        };
        assert_eq!(
            iterable.iter().collect::<Vec<_>>(),
            vec![Value::Number(1.0), Value::Number(2.0)]
        );

        let builtin_iterator = Value::array(vec![Value::string("a"), Value::string("b")])
            .call_method("__w3cos_symbol_iterator", vec![]);
        assert_eq!(
            builtin_iterator.iter().collect::<Vec<_>>(),
            vec![Value::string("a"), Value::string("b")]
        );
    }

    #[test]
    fn string_iterator_yields_unicode_code_points() {
        let iterator = Value::string("A😀").call_method("__w3cos_symbol_iterator", vec![]);
        assert_eq!(
            iterator.call_method("next", vec![]).get_property("value"),
            Value::string("A")
        );
        assert_eq!(
            iterator.call_method("next", vec![]).get_property("value"),
            Value::string("😀")
        );
        assert!(
            iterator
                .call_method("next", vec![])
                .get_property("done")
                .to_bool()
        );
    }

    #[test]
    fn array_aot_and_protocol_iterators_observe_live_length_changes() {
        let array = Value::array(vec![Value::Number(1.0), Value::Number(2.0)]);
        let mut aot = array.iter();
        assert_eq!(aot.next(), Some(Value::Number(1.0)));
        array.call_method("pop", vec![]);
        array.call_method("push", vec![Value::Number(3.0), Value::Number(4.0)]);
        assert_eq!(aot.len(), 2);
        assert_eq!(aot.next(), Some(Value::Number(3.0)));
        assert_eq!(aot.next(), Some(Value::Number(4.0)));
        assert_eq!(aot.next(), None);

        let protocol_array = Value::array(vec![Value::Number(10.0), Value::Number(20.0)]);
        let iterator = protocol_array.call_method("__w3cos_symbol_iterator", vec![]);
        assert_eq!(
            iterator.call_method("next", vec![]).get_property("value"),
            Value::Number(10.0)
        );
        protocol_array.call_method("pop", vec![]);
        protocol_array.call_method("push", vec![Value::Number(30.0)]);
        assert_eq!(
            iterator.call_method("next", vec![]).get_property("value"),
            Value::Number(30.0)
        );
        assert!(
            iterator
                .call_method("next", vec![])
                .get_property("done")
                .to_bool()
        );
    }

    #[test]
    fn typed_array_iterators_read_unvisited_values_from_shared_backing() {
        let typed = crate::binary::typed_array_value(vec![Value::Number(1.0), Value::Number(2.0)]);
        let mut aot = typed.iter();
        assert_eq!(aot.next(), Some(Value::Number(1.0)));
        typed.set_property("1", Value::Number(9.0));
        assert_eq!(aot.next(), Some(Value::Number(9.0)));

        let protocol_typed =
            crate::binary::typed_array_value(vec![Value::Number(10.0), Value::Number(20.0)]);
        let iterator = protocol_typed.call_method("__w3cos_symbol_iterator", vec![]);
        assert_eq!(
            iterator.call_method("next", vec![]).get_property("value"),
            Value::Number(10.0)
        );
        protocol_typed.set_property("1", Value::Number(30.0));
        assert_eq!(
            iterator.call_method("next", vec![]).get_property("value"),
            Value::Number(30.0)
        );
    }

    #[test]
    fn iterator_close_chain_closes_outer_iterators_and_preserves_throw_completion() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let make_iterator = |name: &'static str, throws: bool| {
            let iterator = Value::object(HashMap::new());
            let observed = Rc::clone(&closed);
            iterator.set_property(
                "return",
                Value::function(move |_, _| {
                    observed.borrow_mut().push(name);
                    if throws {
                        throw_value(Value::string(&format!("{name} close")));
                    }
                    Value::Undefined
                }),
            );
            iterator
        };
        let inner = make_iterator("inner", true);
        let outer = make_iterator("outer", false);
        let chain = Value::array(vec![inner, outer]);

        let return_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            chain.call_method("__w3cos_iterator_close_return", vec![])
        }));
        let return_exception = return_outcome
            .unwrap_err()
            .downcast::<PanicValue>()
            .unwrap()
            .0;
        assert_eq!(return_exception, Value::string("inner close"));
        assert_eq!(closed.borrow().as_slice(), &["inner", "outer"]);

        closed.borrow_mut().clear();
        let pending = Value::string("original throw");
        assert_eq!(
            chain.call_method("__w3cos_iterator_close_throw", vec![pending.clone()]),
            pending
        );
        assert_eq!(closed.borrow().as_slice(), &["inner", "outer"]);
    }

    #[test]
    fn iterator_protocol_rejects_non_callable_methods_and_primitive_results() {
        let caught = |operation: &dyn Fn() -> Value| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
                .unwrap_err()
                .downcast::<PanicValue>()
                .unwrap()
                .0
        };

        let non_callable = Value::object(HashMap::new());
        non_callable.set_property("__w3cos_symbol_iterator", Value::Number(1.0));
        assert_eq!(
            caught(&|| non_callable.call_method("__w3cos_symbol_iterator", vec![]))
                .get_property("name"),
            Value::string("TypeError")
        );
        assert_eq!(
            caught(&|| non_callable.iter().next().unwrap_or(Value::Undefined)).get_property("name"),
            Value::string("TypeError")
        );

        let primitive_iterator = Value::object(HashMap::new());
        primitive_iterator.set_property(
            "__w3cos_symbol_iterator",
            Value::function(|_, _| Value::Number(1.0)),
        );
        assert_eq!(
            caught(&|| primitive_iterator.call_method("__w3cos_symbol_iterator", vec![]))
                .get_property("name"),
            Value::string("TypeError")
        );
        assert_eq!(
            caught(&|| primitive_iterator.iter().next().unwrap_or(Value::Undefined))
                .get_property("name"),
            Value::string("TypeError")
        );

        let iterator = Value::object(HashMap::new());
        iterator.set_property(
            "next",
            Value::function(|_, _| Value::string("not an object")),
        );
        assert_eq!(
            caught(&|| iterator.call_method("__w3cos_iterator_next", vec![])).get_property("name"),
            Value::string("TypeError")
        );

        let invalid_return = Value::object(HashMap::new());
        invalid_return.set_property("return", Value::Number(1.0));
        let completion = Value::array(vec![invalid_return]).call_method(
            "__w3cos_iterator_close_throw",
            vec![Value::string("original")],
        );
        assert_eq!(completion.get_property("name"), Value::string("TypeError"));

        let primitive_return = Value::object(HashMap::new());
        primitive_return.set_property("return", Value::function(|_, _| Value::Number(1.0)));
        let return_chain = Value::array(vec![primitive_return]);
        assert_eq!(
            caught(&|| return_chain.call_method("__w3cos_iterator_close_return", vec![]))
                .get_property("name"),
            Value::string("TypeError")
        );
        assert_eq!(
            return_chain.call_method(
                "__w3cos_iterator_close_throw",
                vec![Value::string("original")]
            ),
            Value::string("original")
        );
    }

    #[test]
    fn async_iterator_protocol_validates_acquisition_steps_and_close_priority() {
        let caught = |operation: &dyn Fn() -> Value| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
                .unwrap_err()
                .downcast::<PanicValue>()
                .unwrap()
                .0
        };
        let non_callable = Value::object(HashMap::new());
        non_callable.set_property("__w3cos_symbol_async_iterator", Value::Number(1.0));
        assert_eq!(
            caught(&|| non_callable.call_method("__w3cos_symbol_async_iterator", vec![]))
                .get_property("name"),
            Value::string("TypeError")
        );

        let invalid_next = crate::js_object! {
            "next" => Value::function(|_, _| {
                crate::promise::resolve(vec![Value::Number(1.0)])
            }),
        };
        let next = invalid_next.call_method("__w3cos_async_iterator_next", vec![]);
        crate::promise::drain_microtasks();
        assert!(matches!(
            crate::promise::status(&next),
            Some(crate::promise::PromiseStatus::Rejected(reason))
                if reason.get_property("name") == Value::string("TypeError")
        ));

        let invalid_return = crate::js_object! { "return" => Value::Number(1.0) };
        let close = Value::array(vec![invalid_return]).call_method(
            "__w3cos_async_iterator_close_throw",
            vec![Value::string("body")],
        );
        crate::promise::drain_microtasks();
        assert!(matches!(
            crate::promise::status(&close),
            Some(crate::promise::PromiseStatus::Fulfilled(reason))
                if reason.get_property("name") == Value::string("TypeError")
        ));

        let rejecting_return = crate::js_object! {
            "return" => Value::function(|_, _| {
                crate::promise::reject(vec![Value::string("close")])
            }),
        };
        let close = Value::array(vec![rejecting_return])
            .call_method("__w3cos_async_iterator_close_return", vec![]);
        crate::promise::drain_microtasks();
        assert!(matches!(
            crate::promise::status(&close),
            Some(crate::promise::PromiseStatus::Rejected(reason))
                if reason == Value::string("close")
        ));
    }

    #[test]
    fn property_access() {
        let mut props = HashMap::new();
        props.insert("x".to_string(), Value::Number(42.0));
        let obj = Value::object(props);
        assert_eq!(obj.get_property("x").to_number(), 42.0);
        assert!(obj.get_property("y").is_undefined());

        let arr = Value::array(vec![Value::String("a".into()), Value::String("b".into())]);
        assert_eq!(arr.get_property("0").to_js_string(), "a");
        assert_eq!(arr.get_property("length").to_number(), 2.0);

        let s = Value::String("hello".into());
        assert_eq!(s.get_property("length").to_number(), 5.0);
        assert_eq!(s.get_property("0").to_js_string(), "h");

        let chinese = Value::String("请提前到达，卸货前联系我".into());
        assert_eq!(chinese.get_property("length").to_number(), 12.0);
        assert_eq!(
            chinese
                .call_method("slice", vec![Value::Number(0.0), Value::Number(6.0)])
                .to_js_string(),
            "请提前到达，",
        );
        assert_eq!(
            chinese
                .call_method("slice", vec![Value::Number(-6.0)])
                .to_js_string(),
            "卸货前联系我",
        );
    }

    #[test]
    fn checked_property_access_rejects_nullish_receivers() {
        for receiver in [Value::Undefined, Value::Null] {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                receiver.get_property_checked("next")
            }));
            let payload = outcome.expect_err("nullish member access must throw");
            let error = payload
                .downcast::<PanicValue>()
                .expect("member access throws a JavaScript value")
                .0;
            assert_eq!(error.get_property("name").to_js_string(), "TypeError");
            assert!(
                error
                    .get_property("message")
                    .to_js_string()
                    .contains("reading 'next'")
            );
        }

        let object = crate::js_object! { "next" => Value::Null };
        assert!(object.get_property_checked("next").is_null());
    }

    #[test]
    fn encoded_getters_remain_visible_after_plain_object_fast_path() {
        let object = Value::object(HashMap::new());
        object.set_property(
            "__w3cos_getter_label",
            Value::function(|_, _| Value::from("computed")),
        );

        assert_eq!(object.get_property("label").to_js_string(), "computed");
        assert!(object.get_property("missing").is_undefined());
    }

    #[test]
    fn object_rest_excludes_destructured_own_properties() {
        let object = Value::object(HashMap::from([
            ("ariaAttributes".into(), Value::from("aria")),
            ("style".into(), Value::from("style")),
            ("index".into(), Value::Number(7.0)),
        ]));

        let rest = object.object_rest(&["ariaAttributes", "style"]);

        assert!(rest.get_property("ariaAttributes").is_undefined());
        assert!(rest.get_property("style").is_undefined());
        assert_eq!(rest.get_property("index").to_number(), 7.0);
    }

    #[test]
    fn property_set() {
        let obj = Value::object(HashMap::new());
        obj.set_property("key", Value::Number(99.0));
        assert_eq!(obj.get_property("key").to_number(), 99.0);
    }

    #[test]
    fn delete_removes_object_property() {
        let value = Value::object(HashMap::from([("model".into(), Value::Number(1.0))]));
        assert!(value.delete_property("model").to_bool());
        assert!(value.get_property("model").is_undefined());
        assert!(value.delete_property("model").to_bool());
    }

    #[test]
    fn string_clone_shares_rc_bytes() {
        let a = Value::string("interned-clone-test");
        let b = a.clone();
        match (&a, &b) {
            (Value::Imm(left), Value::Imm(right)) => {
                assert_copy(*left);
                assert_eq!(left.interned_handle(), right.interned_handle());
                assert!(left.is_interned_string());
                assert_eq!(left.as_js_string().unwrap().heap_strong_count(), None);
            }
            _ => panic!("interned short string clone must stay Imm, not Rc"),
        }
        assert_eq!(a, b);
        assert_eq!(a.to_js_string(), "interned-clone-test");
    }

    #[test]
    fn string_intern_reuses_same_literal() {
        let a = Value::string("length");
        let b = Value::string("length");
        match (&a, &b) {
            (Value::Imm(left), Value::Imm(right)) => {
                assert_eq!(left.interned_handle(), right.interned_handle());
            }
            _ => panic!("interned literals must pack into Immediate"),
        }
    }

    #[test]
    fn interned_short_string_value_is_imm_copy_word() {
        let s = Value::string("short-intern-imm");
        assert!(s.is_string());
        match s {
            Value::Imm(word) => {
                let copied = assert_copy(word);
                assert_eq!(std::mem::size_of::<Immediate>(), 8);
                assert_eq!(std::mem::size_of_val(&copied), 8);
                assert!(!std::mem::needs_drop::<Immediate>());
                assert!(copied.is_interned_string());
                let js = copied.as_js_string().expect("handle");
                assert_eq!(js.as_str(), "short-intern-imm");
                assert!(js.page_handle().is_some());
                assert_eq!(js.heap_strong_count(), None);
                assert_eq!(copied.interned_handle(), js.page_handle());
                // tag 5 in low 4 bits, handle in bits 4..36
                assert_eq!(copied.bits() & 0xF, 5);
                assert_eq!(((copied.bits() >> 4) as u32), js.page_handle().unwrap());
            }
            _ => panic!("interned short string Value must be Imm"),
        }
        let cloned = s.clone();
        match cloned {
            Value::Imm(word) => {
                assert_copy(word);
                assert!(word.is_interned_string());
                assert_eq!(word.as_js_string().unwrap().heap_strong_count(), None);
            }
            _ => panic!("clone of interned short string must not take the String arm"),
        }
    }

    #[test]
    fn long_string_value_stays_on_heap_arm() {
        let raw = "L".repeat(257);
        let s = Value::string(&raw);
        assert!(s.is_string());
        assert!(s.as_immediate().is_none());
        match &s {
            Value::String(js) => {
                assert!(js.page_handle().is_none());
                assert_eq!(js.heap_strong_count(), Some(1));
            }
            _ => panic!("long string must stay on the String arm"),
        }
        let cloned = s.clone();
        match (&s, &cloned) {
            (Value::String(left), Value::String(right)) => {
                assert!(left.ptr_eq(right));
                assert_eq!(left.heap_strong_count(), Some(2));
            }
            _ => panic!("long string clone must stay heap"),
        }
        assert_eq!(s.to_js_string(), raw);
    }

    #[test]
    fn object_handle_clone_does_not_increase_rc_and_reset_empties_table() {
        crate::page_arena::reset();
        let obj = Value::object(HashMap::from([("k".into(), Value::Number(1.0))]));
        match &obj {
            Value::Imm(word) => {
                assert!(word.is_object_handle());
                assert!(!std::mem::needs_drop::<Immediate>());
                let handle = word.object_handle().unwrap();
                let js = crate::page_arena::get_object(handle);
                assert_eq!(js.interned_handle(), Some(handle));
                assert!(js.host_strong_count().is_none());
                let cloned = obj.clone();
                let again = obj.as_object().expect("object");
                assert!(js.ptr_eq(&again));
                assert!(js.ptr_eq(&again.clone()));
                match cloned {
                    Value::Imm(copy) => {
                        assert_eq!(copy.object_handle(), word.object_handle())
                    }
                    _ => panic!("clone must stay Imm handle"),
                }
                assert_eq!(obj.get_property("k").to_number(), 1.0);
                assert_eq!(cloned.get_property("k").to_number(), 1.0);
                assert_eq!(word.bits() & 0xF, 6);
            }
            _ => panic!("Value::object must pack a page-arena handle"),
        }
        assert_eq!(crate::page_arena::live_objects(), 1);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_objects(), 0);
    }

    #[test]
    fn array_handle_clone_does_not_increase_rc_and_reset_empties_table() {
        crate::page_arena::reset();
        let arr = Value::array(vec![Value::Number(1.0), Value::Number(2.0)]);
        match &arr {
            Value::Imm(word) => {
                assert!(word.is_array_handle());
                assert!(!std::mem::needs_drop::<Immediate>());
                let handle = word.array_handle().unwrap();
                let js = crate::page_arena::get_array(handle);
                assert_eq!(js.interned_handle(), Some(handle));
                assert!(js.host_strong_count().is_none());
                let cloned = arr.clone();
                let again = arr.as_array().expect("array");
                assert!(js.ptr_eq(&again));
                assert!(js.ptr_eq(&again.clone()));
                match cloned {
                    Value::Imm(copy) => {
                        assert_eq!(copy.array_handle(), word.array_handle())
                    }
                    _ => panic!("clone must stay Imm handle"),
                }
                assert_eq!(arr.get_property("0").to_number(), 1.0);
                assert_eq!(cloned.get_property("1").to_number(), 2.0);
                assert_eq!(arr.get_property("length").to_number(), 2.0);
                assert_eq!(word.bits() & 0xF, 7);
            }
            _ => panic!("Value::array must pack a page-arena handle"),
        }
        assert_eq!(crate::page_arena::live_arrays(), 1);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_arrays(), 0);
    }

    #[test]
    fn function_handle_clone_does_not_increase_rc_and_reset_empties_table() {
        crate::page_arena::reset();
        let func = Value::function(|_, _| Value::Number(7.0));
        match &func {
            Value::Imm(word) => {
                assert!(word.is_function_handle());
                assert!(!std::mem::needs_drop::<Immediate>());
                let handle = word.function_handle().unwrap();
                let js = crate::page_arena::get_function(handle);
                assert_eq!(js.interned_handle(), Some(handle));
                assert!(js.host_strong_count().is_none());
                let cloned = func.clone();
                let again = func.as_function().expect("function");
                assert!(js.ptr_eq(&again));
                assert!(js.ptr_eq(&again.clone()));
                match cloned {
                    Value::Imm(copy) => {
                        assert_eq!(copy.function_handle(), word.function_handle())
                    }
                    _ => panic!("clone must stay Imm handle"),
                }
                assert_eq!(func.call(Value::Undefined, vec![]).to_number(), 7.0);
                assert_eq!(cloned.call(Value::Undefined, vec![]).to_number(), 7.0);
                // tag 8 in low 4 bits
                assert_eq!(word.bits() & 0xF, 8);
            }
            _ => panic!("Value::function must pack a page-arena handle"),
        }
        assert_eq!(crate::page_arena::live_functions(), 1);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_functions(), 0);
    }

    #[test]
    fn interned_function_as_function_clone_is_handle_eq_and_live_table_stays_one() {
        crate::page_arena::reset();
        let func = Value::function(|_, _| Value::Number(3.0));
        assert_eq!(crate::page_arena::live_functions(), 1);
        let a = func.as_function().expect("function");
        let b = a.clone();
        let c = func.as_function().expect("function");
        assert!(a.ptr_eq(&b));
        assert!(a.ptr_eq(&c));
        assert_eq!(a.interned_handle(), func.function_handle());
        assert!(a.host_strong_count().is_none());
        assert!(b.host_strong_count().is_none());
        assert_eq!(crate::page_arena::live_functions(), 1);
        // Nested Value::function during call must not hold the arena RefCell.
        let maker = Value::function(|_, _| Value::function(|_, _| Value::Number(9.0)));
        assert_eq!(crate::page_arena::live_functions(), 2);
        let inner = maker.call(Value::Undefined, vec![]);
        assert!(inner.is_function());
        assert_eq!(inner.call(Value::Undefined, vec![]).to_number(), 9.0);
        assert_eq!(crate::page_arena::live_functions(), 3);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_functions(), 0);
        let stale = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            a.call(Value::Undefined, vec![])
        }));
        assert!(
            stale.is_err(),
            "stale interned function handle must panic after reset"
        );
    }

    #[test]
    fn interned_object_as_object_clone_is_handle_eq_and_live_table_stays_one() {
        crate::page_arena::reset();
        let obj = Value::object(HashMap::from([("k".into(), Value::Number(3.0))]));
        assert_eq!(crate::page_arena::live_objects(), 1);
        let a = obj.as_object().expect("object");
        let b = a.clone();
        let c = obj.as_object().expect("object");
        assert!(a.ptr_eq(&b));
        assert!(a.ptr_eq(&c));
        assert_eq!(a.interned_handle(), obj.object_handle());
        assert!(a.host_strong_count().is_none());
        assert!(b.host_strong_count().is_none());
        assert_eq!(crate::page_arena::live_objects(), 1);
        // Nested Value::object during get/set must not hold the arena RefCell.
        a.borrow_mut().set_direct(
            "child",
            Value::object(HashMap::from([("n".into(), Value::Number(9.0))])),
        );
        assert_eq!(obj.get_property("child").get_property("n").to_number(), 9.0);
        assert_eq!(crate::page_arena::live_objects(), 2);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_objects(), 0);
        let stale = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.borrow().get_direct("k");
        }));
        assert!(
            stale.is_err(),
            "stale interned object handle must panic after reset"
        );
    }

    #[test]
    fn interned_array_as_array_clone_is_handle_eq_and_live_table_stays_one() {
        crate::page_arena::reset();
        let arr = Value::array(vec![Value::Number(3.0)]);
        assert_eq!(crate::page_arena::live_arrays(), 1);
        let a = arr.as_array().expect("array");
        let b = a.clone();
        let c = arr.as_array().expect("array");
        assert!(a.ptr_eq(&b));
        assert!(a.ptr_eq(&c));
        assert_eq!(a.interned_handle(), arr.array_handle());
        assert!(a.host_strong_count().is_none());
        assert!(b.host_strong_count().is_none());
        assert_eq!(crate::page_arena::live_arrays(), 1);
        // Nested Value::array during get/set must not hold the arena RefCell.
        a.borrow_mut().push(Value::array(vec![Value::Number(9.0)]));
        assert_eq!(arr.get_property("1").get_property("0").to_number(), 9.0);
        assert_eq!(crate::page_arena::live_arrays(), 2);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_arrays(), 0);
        let stale = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.borrow().len();
        }));
        assert!(
            stale.is_err(),
            "stale interned array handle must panic after reset"
        );
    }

    #[test]
    fn interned_object_weak_restores_imm_handle_and_fails_after_reset() {
        crate::page_arena::reset();
        let obj = Value::object(HashMap::from([("k".into(), Value::Number(1.0))]));
        let weak = obj.as_object().expect("object").downgrade();
        let upgraded = weak.upgrade_value().expect("live interned object");
        assert!(upgraded.object_handle() == obj.object_handle());
        assert!(matches!(upgraded, Value::Imm(word) if word.is_object_handle()));
        assert_eq!(upgraded.get_property("k").to_number(), 1.0);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_objects(), 0);
        assert!(weak.upgrade_value().is_none());
    }

    #[test]
    fn interned_array_weak_restores_imm_handle_and_fails_after_reset() {
        crate::page_arena::reset();
        let arr = Value::array(vec![Value::Number(1.0)]);
        let weak = arr.as_array().expect("array").downgrade();
        let upgraded = weak.upgrade_value().expect("live interned array");
        assert!(upgraded.array_handle() == arr.array_handle());
        assert!(matches!(upgraded, Value::Imm(word) if word.is_array_handle()));
        assert_eq!(upgraded.get_property("0").to_number(), 1.0);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_arrays(), 0);
        assert!(weak.upgrade_value().is_none());
    }

    #[test]
    fn interned_function_weak_fails_after_reset() {
        crate::page_arena::reset();
        let func = Value::function(|_, _| Value::Number(1.0));
        let weak = func.as_function().expect("function").downgrade();
        let upgraded = weak.upgrade_value().expect("live interned function");
        assert!(upgraded.function_handle() == func.function_handle());
        assert_eq!(upgraded.call(Value::Undefined, vec![]).to_number(), 1.0);
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_functions(), 0);
        assert!(weak.upgrade_value().is_none());
    }

    #[test]
    fn interned_string_handles_drop_on_page_reset() {
        crate::page_arena::reset();
        let s = Value::string("reset-drops-imm-handle");
        assert!(matches!(s, Value::Imm(word) if word.is_interned_string()));
        crate::page_arena::reset();
        assert_eq!(crate::page_arena::live_handles(), 0);
        assert_eq!(JsString::interned_table_bytes(), 0);
    }
}
