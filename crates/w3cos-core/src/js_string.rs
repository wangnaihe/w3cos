use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

use crate::page_arena::{self, PageString};

/// Interned / shared JavaScript string.
///
/// Short strings (`<=` [`INTERN_LIMIT`]) live in the page bump arena and
/// clone as a `u32` handle. Longer strings still share via `Rc<str>` so a
/// unique large payload cannot pin the intern map for the rest of the page.
///
/// Core heap values are thread-confined (`Rc<RefCell<JsObject>>`, thread-local
/// heap counters), so the intern table is thread-local as well. The page bump
/// is dropped on `page_arena::reset` (`reset_bridge` / navigation).
///
/// This is a Core type. `w3cos-dom`'s `Atom` stays in DOM; Core must not
/// depend on that crate. VM and AOT share this representation — there is no
/// second AOT-only string type.
pub struct JsString(Repr);

enum Repr {
    Interned(PageString),
    Heap(Rc<str>),
}

/// Property-key / short-literal intern cutoff. Longer strings still share
/// via `Rc` on `Value` clone, but skip the page intern table.
const INTERN_LIMIT: usize = 256;

impl Clone for JsString {
    fn clone(&self) -> Self {
        match &self.0 {
            Repr::Interned(interned) => Self(Repr::Interned(*interned)),
            Repr::Heap(rc) => Self(Repr::Heap(Rc::clone(rc))),
        }
    }
}

impl JsString {
    pub fn intern(s: &str) -> Self {
        if s.len() > INTERN_LIMIT {
            return Self(Repr::Heap(Rc::from(s)));
        }
        Self(Repr::Interned(page_arena::intern(s)))
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Interned(interned) => interned.as_str(),
            Repr::Heap(rc) => rc,
        }
    }

    /// True when both handles share the same intern slot or `Rc` allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Interned(a), Repr::Interned(b)) => a.handle == b.handle && a.epoch == b.epoch,
            (Repr::Heap(a), Repr::Heap(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn interned_table_bytes() -> usize {
        page_arena::allocated_bytes()
    }

    /// Page intern handle when this string is page-local (not a long `Rc`).
    pub fn page_handle(&self) -> Option<u32> {
        match self.0 {
            Repr::Interned(interned) => Some(interned.handle),
            Repr::Heap(_) => None,
        }
    }

    /// `Rc` strong-count for long heap strings. `None` for page-interned
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
    fn long_strings_still_clone_without_copying_via_rc() {
        let raw = "x".repeat(INTERN_LIMIT + 8);
        let a = JsString::intern(&raw);
        assert!(a.page_handle().is_none());
        assert_eq!(a.heap_strong_count(), Some(1));
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        assert_eq!(a.as_str(), raw);
        assert_eq!(a.heap_strong_count(), Some(2));
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
}
