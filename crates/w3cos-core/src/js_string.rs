use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

/// Interned / shared JavaScript string.
///
/// `Clone` increments an `Rc`; it does not copy UTF-8 bytes. Core heap values
/// are already thread-confined (`Rc<RefCell<JsObject>>`, thread-local heap
/// counters), so this intern table is thread-local as well.
///
/// Strings longer than [`INTERN_LIMIT`] are still `Rc<str>` (cheap clone) but
/// are not entered in the table, so unique large payloads cannot pin the
/// intern map for the rest of the thread.
///
/// This is a Core type. `w3cos-dom`'s `Atom` stays in DOM; Core must not
/// depend on that crate. VM and AOT share this representation — there is no
/// second AOT-only string type.
#[derive(Clone)]
pub struct JsString(Rc<str>);

/// Property-key / short-literal intern cutoff. Longer strings still share
/// via `Rc` on `Value` clone, but skip the process-thread table.
const INTERN_LIMIT: usize = 256;

impl JsString {
    pub fn intern(s: &str) -> Self {
        if s.len() > INTERN_LIMIT {
            return Self(Rc::from(s));
        }
        INTERN_TABLE.with(|table| {
            let mut table = table.borrow_mut();
            if let Some(existing) = table.by_str.get(s) {
                return Self(Rc::clone(existing));
            }
            let rc: Rc<str> = Rc::from(s);
            table.by_str.insert(Rc::clone(&rc), Rc::clone(&rc));
            Self(rc)
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when both handles share the same `Rc` allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn interned_table_bytes() -> usize {
        INTERN_TABLE.with(|table| {
            let table = table.borrow();
            table
                .by_str
                .keys()
                .map(|s| s.len())
                .fold(0usize, usize::saturating_add)
        })
    }
}

impl Deref for JsString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for JsString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for JsString {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
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
        self.0.as_ref().cmp(other.0.as_ref())
    }
}

impl Hash for JsString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        *self.0 == *other
    }
}

impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        *self.0 == **other
    }
}

impl PartialEq<String> for JsString {
    fn eq(&self, other: &String) -> bool {
        *self.0 == **other
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

struct InternTable {
    by_str: HashMap<Rc<str>, Rc<str>>,
}

impl InternTable {
    fn new() -> Self {
        let mut table = Self {
            by_str: HashMap::new(),
        };
        // Common JS / object keys. Sharing is by `Rc` identity.
        const PREINTERNED: &[&str] = &[
            "",
            "length",
            "prototype",
            "constructor",
            "name",
            "message",
            "stack",
            "value",
            "writable",
            "enumerable",
            "configurable",
            "get",
            "set",
            "toString",
            "valueOf",
            "undefined",
            "null",
            "true",
            "false",
            "NaN",
            "Infinity",
            "object",
            "function",
            "string",
            "number",
            "boolean",
            "symbol",
            "bigint",
        ];
        for s in PREINTERNED {
            let rc: Rc<str> = Rc::from(*s);
            table.by_str.insert(Rc::clone(&rc), rc);
        }
        table
    }
}

thread_local! {
    static INTERN_TABLE: RefCell<InternTable> = RefCell::new(InternTable::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_same_string_shares_rc() {
        let a = JsString::intern("length");
        let b = JsString::intern("length");
        assert!(a.ptr_eq(&b));
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "length");
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
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        assert_eq!(a.as_str(), raw);
    }

    #[test]
    fn hashmap_lookup_by_str() {
        let mut map = HashMap::new();
        map.insert(JsString::intern("value"), 1u32);
        assert_eq!(map.get("value"), Some(&1));
    }
}
