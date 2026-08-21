//! Page-scoped bump arena and integer string handles.
//!
//! Short interned JS strings live here so clones copy a `u32` handle instead
//! of `Rc::clone`. The bump is dropped on [`reset`] (`reset_bridge` /
//! document navigation). Size-class slabs wait for a later cut.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
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
            panic!("page-interned string used after reset_bridge");
        }
        // Safety: `ptr`/`len` name UTF-8 bytes in a bump chunk that lives
        // until `reset` bumps the epoch. Callers must not hold the `&str` across
        // a page reset.
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len as usize))
        }
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

fn current_epoch() -> u32 {
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

/// Resolve a live intern handle. Panics if the handle belongs to a
/// previous page (same contract as [`PageString::as_str`]).
pub(crate) fn get(handle: u32) -> Option<PageString> {
    if handle == 0 {
        return None;
    }
    ARENA.with(|arena| {
        let arena = arena.borrow();
        let Some(slot) = arena.slots.get(handle as usize).copied() else {
            panic!("page-interned string used after reset_bridge");
        };
        if slot.handle != handle || slot.epoch != arena.epoch {
            panic!("page-interned string used after reset_bridge");
        }
        Some(slot)
    })
}

/// Drop interned page strings. Called from `reset_bridge` / navigation.
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

/// Live intern handles (empty after [`reset`]).
pub fn live_handles() -> usize {
    ARENA.with(|arena| arena.borrow().slots.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_reuses_handle_and_reset_empties_table() {
        reset();
        assert_eq!(allocated_bytes(), 0);
        assert_eq!(live_handles(), 0);

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
        assert_eq!(allocated_bytes(), 0);
        assert_eq!(live_handles(), 0);

        let again = intern("page-arena-unique-key");
        assert_eq!(again.as_str(), "page-arena-unique-key");
        assert_ne!(again.epoch, a.epoch);
    }
}
