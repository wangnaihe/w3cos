use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

use crate::page_arena;

/// Interned / shared JavaScript string.
///
/// Page-local strings from [`Self::intern`] live in the page bump arena and
/// clone as a `(handle, epoch)` word (any length). Host / outliving strings
/// use [`Self::heap`] (`Rc<String>`, thin pointer) so they survive
/// `page_arena::reset`.
///
/// Both arms are pointer-sized so [`JsString`] is 16 bytes on 64-bit. That
/// keeps [`crate::Value::String`] from inflating [`crate::Value`] past two
/// words (host `Rc` arms + NaN-box [`crate::Immediate`] already fit in 16).
/// Fat `PageString` (cached bump ptr/len) stays inside the arena slots only.
///
/// Core heap values are thread-confined (`Rc<RefCell<JsObject>>`, thread-local
/// heap counters), so the intern table is thread-local as well. The page bump
/// is dropped on `page_arena::reset` (`reset_bridge` / navigation). Stale
/// page handles yield empty `as_str` (no panic), matching short-string
/// leftover Values after navigation.
///
/// This is a Core type. `w3cos-dom`'s `Atom` stays in DOM; Core must not
/// depend on that crate. VM and AOT share this representation — there is no
/// second AOT-only string type.
pub struct JsString(Repr);

/// Page-local handle word. Bump bytes are resolved through the arena on
/// [`JsString::as_str`]; we deliberately do not cache `ptr`/`len` here so
/// the `Value::String` arm stays one word of payload.
#[derive(Clone, Copy)]
struct InternedHandle {
    handle: u32,
    epoch: u32,
}

enum Repr {
    Interned(InternedHandle),
    /// Thin `Rc` (sized `String`) — `Rc<str>` would be a fat pointer and
    /// blow [`JsString`] / [`crate::Value`] back to 24–32 bytes.
    Heap(Rc<String>),
}

impl Clone for JsString {
    fn clone(&self) -> Self {
        match &self.0 {
            Repr::Interned(interned) => Self(Repr::Interned(*interned)),
            Repr::Heap(rc) => Self(Repr::Heap(Rc::clone(rc))),
        }
    }
}

impl JsString {
    /// Page-arena intern (any length). Clone is a handle copy; bytes drop
    /// on `page_arena::reset`. Prefer [`Self::heap`] for host strings that
    /// must outlive the page.
    pub fn intern(s: &str) -> Self {
        // Arena slot `len` is `u32`; absurd sizes fall back to heap Rc.
        if s.len() > u32::MAX as usize {
            return Self::heap(s);
        }
        let page = page_arena::intern(s);
        Self(Repr::Interned(InternedHandle {
            handle: page.handle,
            epoch: page.epoch,
        }))
    }

    /// Host / outliving string: thin `Rc<String>`, not page-local. Survives
    /// `page_arena::reset` (unlike [`Self::intern`]).
    pub fn heap(s: impl AsRef<str>) -> Self {
        Self(Repr::Heap(Rc::new(s.as_ref().to_string())))
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Interned(interned) => page_arena::str_at(interned.handle, interned.epoch),
            Repr::Heap(rc) => rc.as_str(),
        }
    }

    /// True when both handles share the same intern slot or `Rc` allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Interned(a), Repr::Interned(b)) => {
                a.handle == b.handle && a.epoch == b.epoch
            }
            (Repr::Heap(a), Repr::Heap(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn interned_table_bytes() -> usize {
        page_arena::allocated_bytes()
    }

    /// Page intern handle when this string is page-local (not a heap `Rc`).
    pub fn page_handle(&self) -> Option<u32> {
        match self.0 {
            Repr::Interned(interned) => Some(interned.handle),
            Repr::Heap(_) => None,
        }
    }

    /// Rebuild a page-interned `JsString` from a NaN-box handle.
    pub(crate) fn from_page_handle(handle: u32) -> Option<Self> {
        let page = page_arena::get(handle)?;
        Some(Self(Repr::Interned(InternedHandle {
            handle: page.handle,
            epoch: page.epoch,
        })))
    }

    /// `Rc` strong-count for [`Self::heap`] strings. `None` for page-interned
    /// handles, which do not clone through `Rc`.
    pub fn heap_strong_count(&self) -> Option<usize> {
        match &self.0 {
            Repr::Interned(_) => None,
            Repr::Heap(rc) => Some(Rc::strong_count(rc)),
        }
    }
}

impl Deref for JsString {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for JsString {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for JsString {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || self.as_str() == other.as_str()
    }
}

impl Eq for JsString {}

impl PartialOrd for JsString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsString {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for JsString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for JsString {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for JsString {
    fn from(value: &str) -> Self {
        Self::intern(value)
    }
}

impl From<String> for JsString {
    fn from(value: String) -> Self {
        Self::intern(&value)
    }
}

impl From<&String> for JsString {
    fn from(value: &String) -> Self {
        Self::intern(value)
    }
}

impl From<JsString> for String {
    fn from(value: JsString) -> Self {
        value.as_str().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn intern_same_string_shares_handle() {
        let a = JsString::intern("length");
        let b = JsString::intern("length");
        assert!(a.ptr_eq(&b));
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "length");
        assert_eq!(a.page_handle(), b.page_handle());
        assert!(a.page_handle().is_some());
    }

    #[test]
    fn interned_clone_is_handle_copy_not_rc() {
        let a = JsString::intern("page-local-prop-key");
        assert!(a.page_handle().is_some());
        assert_eq!(a.heap_strong_count(), None);
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        assert_eq!(a.page_handle(), b.page_handle());
        assert_eq!(b.heap_strong_count(), None);
    }

    #[test]
    fn different_strings_are_not_equal() {
        assert_ne!(JsString::intern("length"), JsString::intern("value"));
    }

    #[test]
    fn clone_is_pointer_eq() {
        let a = JsString::intern("prototype");
        let b = a.clone();
        assert!(a.ptr_eq(&b));
    }

    #[test]
    fn long_page_strings_intern_as_handle_not_rc() {
        let raw = "x".repeat(512);
        let a = JsString::intern(&raw);
        assert!(a.page_handle().is_some());
        assert_eq!(a.heap_strong_count(), None);
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        assert_eq!(a.as_str(), raw);
        assert_eq!(a.page_handle(), b.page_handle());
        assert_eq!(a.heap_strong_count(), None);
    }

    #[test]
    fn heap_strings_stay_rc_and_survive_page_reset() {
        page_arena::reset();
        let raw = "heap-outlives-page-".to_string() + &"y".repeat(300);
        let a = JsString::heap(&raw);
        assert!(a.page_handle().is_none());
        assert_eq!(a.heap_strong_count(), Some(1));
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        assert_eq!(a.heap_strong_count(), Some(2));
        page_arena::reset();
        assert_eq!(a.as_str(), raw);
        assert_eq!(b.as_str(), raw);
        assert_eq!(a.heap_strong_count(), Some(2));
    }

    #[test]
    fn long_interned_string_as_str_empty_after_reset() {
        page_arena::reset();
        let raw = "L".repeat(400);
        let a = JsString::intern(&raw);
        assert_eq!(a.as_str(), raw);
        let handle = a.page_handle().expect("page handle");
        page_arena::reset();
        assert_eq!(page_arena::live_handles(), 0);
        assert_eq!(a.as_str(), "");
        assert!(JsString::from_page_handle(handle).is_none());
    }

    #[test]
    fn hashmap_lookup_by_str() {
        let mut map = HashMap::new();
        map.insert(JsString::intern("value"), 1u32);
        assert_eq!(map.get("value"), Some(&1));
    }

    #[test]
    fn interned_bytes_drop_on_page_reset() {
        page_arena::reset();
        assert_eq!(page_arena::live_handles(), 0);
        assert_eq!(page_arena::allocated_bytes(), 0);
        let _s = JsString::intern("reset-drops-this-key");
        assert!(page_arena::live_handles() >= 1);
        assert!(page_arena::allocated_bytes() >= "reset-drops-this-key".len());
        page_arena::reset();
        assert_eq!(page_arena::live_handles(), 0);
        assert_eq!(page_arena::allocated_bytes(), 0);
        assert_eq!(JsString::interned_table_bytes(), 0);
    }

    #[test]
    fn js_string_is_two_words_on_64bit() {
        assert_eq!(std::mem::size_of::<JsString>(), 16);
        assert_eq!(std::mem::align_of::<JsString>(), 8);
    }
}
