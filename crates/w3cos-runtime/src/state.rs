use std::cell::RefCell;

/// Thread-local reactive signal store.
/// Signals hold i64 values that can be read and modified by event handlers.
/// When any signal changes, the `dirty` flag is set, prompting a UI rebuild.
struct SignalStore {
    values: Vec<i64>,
    dirty: bool,
}

impl SignalStore {
    fn new() -> Self {
        Self {
            values: Vec::new(),
            dirty: false,
        }
    }
}

thread_local! {
    static STORE: RefCell<SignalStore> = RefCell::new(SignalStore::new());
}

/// Create a new signal with the given initial value. Returns the signal ID.
pub fn create_signal(initial: i64) -> usize {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        let id = store.values.len();
        store.values.push(initial);
        id
    })
}

/// Read the current value of a signal.
pub fn get_signal(id: usize) -> i64 {
    STORE.with(|s| s.borrow().values.get(id).copied().unwrap_or(0))
}

/// Set a signal to a new value. Marks the store as dirty.
pub fn set_signal(id: usize, value: i64) {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        if let Some(slot) = store.values.get_mut(id)
            && *slot != value
        {
            *slot = value;
            store.dirty = true;
        }
    })
}

/// Check if any signal has been modified since the last `clear_dirty()`.
pub fn is_dirty() -> bool {
    STORE.with(|s| s.borrow().dirty)
}

/// Clear the dirty flag after a UI rebuild.
pub fn clear_dirty() {
    STORE.with(|s| s.borrow_mut().dirty = false)
}

/// Reset the entire store (used between rebuilds to re-register signals idempotently).
pub fn reset() {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        store.dirty = false;
    })
}

/// Execute an event action against the signal store.
pub fn execute_action(action: &w3cos_std::EventAction) {
    match action {
        w3cos_std::EventAction::None => {}
        w3cos_std::EventAction::Increment(id) => {
            let val = get_signal(*id);
            set_signal(*id, val + 1);
        }
        w3cos_std::EventAction::Decrement(id) => {
            let val = get_signal(*id);
            set_signal(*id, val - 1);
        }
        w3cos_std::EventAction::Set(id, value) => {
            set_signal(*id, *value);
        }
        w3cos_std::EventAction::Toggle(id) => {
            let val = get_signal(*id);
            set_signal(*id, if val == 0 { 1 } else { 0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read_signal() {
        let id = create_signal(42);
        assert_eq!(get_signal(id), 42);
    }

    #[test]
    fn set_signal_marks_dirty() {
        let id = create_signal(0);
        clear_dirty();
        assert!(!is_dirty());
        set_signal(id, 10);
        assert!(is_dirty());
        assert_eq!(get_signal(id), 10);
    }

    #[test]
    fn set_same_value_not_dirty() {
        let id = create_signal(5);
        clear_dirty();
        set_signal(id, 5);
        assert!(!is_dirty());
    }

    #[test]
    fn execute_increment_action() {
        let id = create_signal(0);
        clear_dirty();
        execute_action(&w3cos_std::EventAction::Increment(id));
        assert_eq!(get_signal(id), 1);
        assert!(is_dirty());
    }

    #[test]
    fn execute_decrement_action() {
        let id = create_signal(10);
        clear_dirty();
        execute_action(&w3cos_std::EventAction::Decrement(id));
        assert_eq!(get_signal(id), 9);
    }

    #[test]
    fn execute_toggle_action() {
        let id = create_signal(0);
        execute_action(&w3cos_std::EventAction::Toggle(id));
        assert_eq!(get_signal(id), 1);
        execute_action(&w3cos_std::EventAction::Toggle(id));
        assert_eq!(get_signal(id), 0);
    }
}
