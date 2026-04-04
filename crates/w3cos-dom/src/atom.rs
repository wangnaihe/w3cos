use std::collections::HashMap;

/// An interned string handle. Comparison is O(1) integer equality.
/// Inspired by Chrome/Blink's WTF::AtomString.
#[derive(Clone, Copy, Eq, Hash)]
pub struct Atom(u32);

impl serde::Serialize for Atom {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Atom {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Atom::intern(&s))
    }
}

use serde::Deserialize;

impl Atom {
    pub const EMPTY: Self = Self(0);

    pub fn intern(s: &str) -> Self {
        INTERN_TABLE.with(|t| {
            let mut table = t.borrow_mut();
            if let Some(&id) = table.str_to_id.get(s) {
                return Self(id);
            }
            let id = table.strings.len() as u32;
            let owned = s.to_string();
            table.str_to_id.insert(owned.clone(), id);
            table.strings.push(owned);
            Self(id)
        })
    }

    pub fn as_str(&self) -> String {
        INTERN_TABLE.with(|t| {
            let table = t.borrow();
            table.strings.get(self.0 as usize).cloned().unwrap_or_default()
        })
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl PartialEq for Atom {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl std::fmt::Debug for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Atom({}, {:?})", self.0, self.as_str())
    }
}

impl std::fmt::Display for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

struct InternTable {
    strings: Vec<String>,
    str_to_id: HashMap<String, u32>,
}

impl InternTable {
    fn new() -> Self {
        let mut t = Self {
            strings: Vec::new(),
            str_to_id: HashMap::new(),
        };
        // Pre-intern common strings (index 0 = empty)
        let preinterned = [
            "",           // 0
            "#document",  // 1
            "#text",      // 2
            "body",       // 3
            "div",        // 4
            "span",       // 5
            "p",          // 6
            "button",     // 7
            "input",      // 8
            "a",          // 9
            "img",        // 10
            "h1",         // 11
            "h2",         // 12
            "h3",         // 13
            "h4",         // 14
            "h5",         // 15
            "h6",         // 16
            "section",    // 17
            "main",       // 18
            "article",    // 19
            "nav",        // 20
            "header",     // 21
            "footer",     // 22
            "aside",      // 23
            "form",       // 24
            "label",      // 25
            "ul",         // 26
            "ol",         // 27
            "li",         // 28
            "em",         // 29
            "strong",     // 30
            "code",       // 31
            "pre",        // 32
            "table",      // 33
            "tr",         // 34
            "td",         // 35
            "th",         // 36
            "id",         // 37
            "class",      // 38
            "src",        // 39
            "href",       // 40
            "type",       // 41
            "placeholder",// 42
            "style",      // 43
            "value",      // 44
        ];
        for s in &preinterned {
            let id = t.strings.len() as u32;
            t.str_to_id.insert(s.to_string(), id);
            t.strings.push(s.to_string());
        }
        t
    }
}

thread_local! {
    static INTERN_TABLE: std::cell::RefCell<InternTable> = std::cell::RefCell::new(InternTable::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_same_string_returns_same_id() {
        let a = Atom::intern("div");
        let b = Atom::intern("div");
        assert_eq!(a, b);
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn different_strings_different_ids() {
        let a = Atom::intern("div");
        let b = Atom::intern("span");
        assert_ne!(a, b);
    }

    #[test]
    fn as_str_roundtrip() {
        let a = Atom::intern("custom-element");
        assert_eq!(a.as_str(), "custom-element");
    }

    #[test]
    fn preinterned_common_tags() {
        let div = Atom::intern("div");
        let body = Atom::intern("body");
        assert_eq!(div.as_str(), "div");
        assert_eq!(body.as_str(), "body");
        // Pre-interned should have low IDs
        assert!(div.0 < 50);
        assert!(body.0 < 50);
    }

    #[test]
    fn empty_atom() {
        assert_eq!(Atom::EMPTY.as_str(), "");
    }
}
