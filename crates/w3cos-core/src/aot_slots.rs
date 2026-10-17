//! Frame slots for AOT functions whose binding ids are sparse.
//!
//! Binding ids are known when the function is compiled, so the frame keeps
//! them in a sorted static table and the values in a compact vector. This is
//! the same idea as V8's register file / Blink's ScriptState-free bytecode
//! frames: a slot is an index, not a hashed key. The method names match
//! `HashMap` so the emitted code reads the same for every storage shape.

/// Values indexed by a sorted, compile-time list of binding ids.
pub struct SparseSlots<T> {
    ids: &'static [u32],
    slots: Vec<Option<T>>,
}

impl<T> SparseSlots<T> {
    #[inline]
    pub fn new(ids: &'static [u32]) -> Self {
        debug_assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        let mut slots = Vec::with_capacity(ids.len());
        slots.resize_with(ids.len(), || None);
        Self { ids, slots }
    }

    #[inline]
    fn index(&self, id: u32) -> Option<usize> {
        self.ids.binary_search(&id).ok()
    }

    #[inline]
    pub fn get(&self, id: &u32) -> Option<&T> {
        let index = self.index(*id)?;
        self.slots[index].as_ref()
    }

    #[inline]
    pub fn get_mut(&mut self, id: &u32) -> Option<&mut T> {
        let index = self.index(*id)?;
        self.slots[index].as_mut()
    }

    /// Ids outside the table are never read by the frame (captures maps may
    /// carry the enclosing function's other bindings), so they are dropped.
    #[inline]
    pub fn insert(&mut self, id: u32, value: T) -> Option<T> {
        let index = self.index(id)?;
        self.slots[index].replace(value)
    }
}

#[cfg(test)]
mod tests {
    use super::SparseSlots;

    #[test]
    fn sparse_slots_behave_like_a_map_for_known_ids() {
        let mut slots = SparseSlots::new(&[3, 40, 977]);
        assert!(slots.get(&40).is_none());
        assert_eq!(slots.insert(40, "a"), None);
        assert_eq!(slots.insert(40, "b"), Some("a"));
        assert_eq!(slots.get(&40), Some(&"b"));
        *slots.get_mut(&40).unwrap() = "c";
        assert_eq!(slots.get(&40), Some(&"c"));
        assert_eq!(slots.insert(5, "x"), None);
        assert!(slots.get(&5).is_none());
        assert!(slots.get(&977).is_none());
    }
}
