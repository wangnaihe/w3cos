/// An interned string handle. Comparison is O(1) integer equality.
/// Inspired by Chrome/Blink's WTF::AtomString.
///
/// Bytes live once in a chunked bump arena. Lookup is open-addressed, so
/// interning a new string does not allocate a hash-map node.
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
        INTERN_TABLE.with(|t| Self(t.borrow_mut().intern(s)))
    }

    pub fn as_str(&self) -> String {
        self.as_static().to_string()
    }

    /// Interned bytes. The arena never relocates or frees a stored string.
    pub fn as_static(self) -> &'static str {
        INTERN_TABLE.with(|t| t.borrow().as_static(self.0))
    }

    /// Borrow the interned bytes for the duration of `f`.
    pub fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        f(self.as_static())
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn from_u32(id: u32) -> Self {
        Self(id)
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

const CHUNK: usize = 8192;
const SLOT_CAP: usize = 32768;
/// Spare chunk boxes so interning unique ids does not malloc on the create path.
const SPARE_CHUNKS: usize = 48;

struct InternTable {
    chunks: Vec<Box<[u8; CHUNK]>>,
    /// Bytes used in the last chunk.
    fill: usize,
    /// Strings longer than a chunk. Each box stays put for the process lifetime.
    wide: Vec<Box<[u8]>>,
    /// `(chunk, offset, len)` per atom id. Chunk indexes with the high bit set
    /// refer to `wide` instead of `chunks`.
    ranges: Vec<(u32, u32, u32)>,
    /// Prebuilt chunk boxes. `push_bytes` pops one instead of allocating.
    spares: Vec<Box<[u8; CHUNK]>>,
    /// Open-addressed ids. `0` is empty; a stored id is `atom + 1`.
    slots: Vec<u32>,
    mask: usize,
    len: usize,
}

fn hash_str(s: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl InternTable {
    fn new() -> Self {
        let mut t = Self {
            chunks: Vec::new(),
            fill: CHUNK,
            wide: Vec::new(),
            ranges: Vec::with_capacity(SLOT_CAP),
            spares: (0..SPARE_CHUNKS).map(|_| Box::new([0; CHUNK])).collect(),
            slots: vec![0; SLOT_CAP],
            mask: SLOT_CAP - 1,
            len: 0,
        };
        // Pre-intern common strings (index 0 = empty)
        let preinterned = [
            "",            // 0
            "#document",   // 1
            "#text",       // 2
            "body",        // 3
            "div",         // 4
            "span",        // 5
            "p",           // 6
            "button",      // 7
            "input",       // 8
            "a",           // 9
            "img",         // 10
            "h1",          // 11
            "h2",          // 12
            "h3",          // 13
            "h4",          // 14
            "h5",          // 15
            "h6",          // 16
            "section",     // 17
            "main",        // 18
            "article",     // 19
            "nav",         // 20
            "header",      // 21
            "footer",      // 22
            "aside",       // 23
            "form",        // 24
            "label",       // 25
            "ul",          // 26
            "ol",          // 27
            "li",          // 28
            "em",          // 29
            "strong",      // 30
            "code",        // 31
            "pre",         // 32
            "table",       // 33
            "tr",          // 34
            "td",          // 35
            "th",          // 36
            "id",          // 37
            "class",       // 38
            "src",         // 39
            "href",        // 40
            "type",        // 41
            "placeholder", // 42
            "style",       // 43
            "value",       // 44
        ];
        for s in &preinterned {
            t.intern(s);
        }
        t
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(id) = self.lookup(s) {
            return id;
        }
        if (self.len + 1) * 10 >= self.slots.len() * 7 {
            self.grow();
        }
        let id = self.ranges.len() as u32;
        self.push_bytes(s.as_bytes());
        self.insert_slot(hash_str(s), id);
        self.len += 1;
        id
    }

    fn lookup(&self, s: &str) -> Option<u32> {
        if self.slots.is_empty() {
            return None;
        }
        let mut index = hash_str(s) as usize & self.mask;
        for _ in 0..self.slots.len() {
            let slot = self.slots[index];
            if slot == 0 {
                return None;
            }
            let id = slot - 1;
            if self.eq(id, s) {
                return Some(id);
            }
            index = (index + 1) & self.mask;
        }
        None
    }

    fn eq(&self, id: u32, s: &str) -> bool {
        self.as_static(id) == s
    }

    fn as_static(&self, id: u32) -> &'static str {
        let Some(&(chunk, offset, len)) = self.ranges.get(id as usize) else {
            return "";
        };
        if len == 0 {
            return "";
        }
        let ptr = if chunk & 0x8000_0000 != 0 {
            self.wide[(chunk & 0x7fff_ffff) as usize].as_ptr()
        } else {
            self.chunks[chunk as usize].as_ptr()
        };
        // Safety: chunk boxes are never relocated or mutated after the bytes
        // are published. `as_static` is only called once the range exists.
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                ptr.add(offset as usize),
                len as usize,
            ))
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if bytes.len() > CHUNK {
            let mut wide = vec![0u8; bytes.len()].into_boxed_slice();
            wide.copy_from_slice(bytes);
            let len = wide.len() as u32;
            self.wide.push(wide);
            let chunk = (self.wide.len() - 1) as u32;
            self.ranges.push((0x8000_0000 | chunk, 0, len));
            return;
        }
        if self.chunks.is_empty() || self.fill + bytes.len() > CHUNK {
            let chunk = self
                .spares
                .pop()
                .unwrap_or_else(|| Box::new([0; CHUNK]));
            self.chunks.push(chunk);
            self.fill = 0;
        }
        let chunk_index = self.chunks.len() - 1;
        let start = self.fill;
        self.chunks[chunk_index][start..start + bytes.len()].copy_from_slice(bytes);
        self.ranges
            .push((chunk_index as u32, start as u32, bytes.len() as u32));
        self.fill += bytes.len();
    }

    fn insert_slot(&mut self, hash: u64, id: u32) {
        let mut index = hash as usize & self.mask;
        loop {
            if self.slots[index] == 0 {
                self.slots[index] = id + 1;
                return;
            }
            index = (index + 1) & self.mask;
        }
    }

    fn grow(&mut self) {
        let next = (self.slots.len() * 2).max(SLOT_CAP);
        self.slots = vec![0; next];
        self.mask = next - 1;
        let ids = self.ranges.len();
        for id in 0..ids {
            let text = self.as_static(id as u32);
            let hash = hash_str(text);
            self.insert_slot(hash, id as u32);
        }
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
