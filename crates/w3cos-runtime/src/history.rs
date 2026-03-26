use std::cell::RefCell;
use w3cos_dom::history::History;
use w3cos_dom::location::Location;

struct HistoryStore {
    history: History,
    location: Location,
    popstate_handlers: Vec<fn(Option<String>)>,
    hashchange_handlers: Vec<fn(String, String)>,
}

impl HistoryStore {
    fn new() -> Self {
        Self {
            history: History::new(),
            location: Location::new("/"),
            popstate_handlers: Vec::new(),
            hashchange_handlers: Vec::new(),
        }
    }

    /// Synchronise the Location fields from the current history URL.
    fn sync_location(&mut self) {
        let url = self.history.current_url().to_string();
        self.location.set_href(&url);
    }
}

thread_local! {
    static STORE: RefCell<HistoryStore> = RefCell::new(HistoryStore::new());
}

fn mark_dirty() {
    crate::state::STORE.with(|s| s.borrow_mut().dirty = true);
}

// ---------------------------------------------------------------------------
// History API
// ---------------------------------------------------------------------------

pub fn push_state(state: Option<&str>, title: &str, url: &str) {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        store.history.push_state(state.map(|s| s.to_string()), title, url);
        store.sync_location();
    });
    mark_dirty();
}

pub fn replace_state(state: Option<&str>, title: &str, url: &str) {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        store.history.replace_state(state.map(|s| s.to_string()), title, url);
        store.sync_location();
    });
    mark_dirty();
}

pub fn back() {
    let (navigated, state_val) = STORE.with(|s| {
        let mut store = s.borrow_mut();
        let nav = store.history.back();
        if nav {
            store.sync_location();
        }
        (nav, store.history.state().map(|s| s.to_string()))
    });
    if navigated {
        mark_dirty();
        fire_popstate(state_val);
    }
}

pub fn forward() {
    let (navigated, state_val) = STORE.with(|s| {
        let mut store = s.borrow_mut();
        let nav = store.history.forward();
        if nav {
            store.sync_location();
        }
        (nav, store.history.state().map(|s| s.to_string()))
    });
    if navigated {
        mark_dirty();
        fire_popstate(state_val);
    }
}

pub fn go(delta: i32) {
    let (navigated, state_val) = STORE.with(|s| {
        let mut store = s.borrow_mut();
        let nav = store.history.go(delta);
        if nav {
            store.sync_location();
        }
        (nav, store.history.state().map(|s| s.to_string()))
    });
    if navigated {
        mark_dirty();
        fire_popstate(state_val);
    }
}

// ---------------------------------------------------------------------------
// Location getters
// ---------------------------------------------------------------------------

pub fn get_pathname() -> String {
    STORE.with(|s| s.borrow().location.pathname().to_string())
}

pub fn get_hash() -> String {
    STORE.with(|s| s.borrow().location.hash().to_string())
}

pub fn get_search() -> String {
    STORE.with(|s| s.borrow().location.search().to_string())
}

pub fn get_href() -> String {
    STORE.with(|s| s.borrow().location.href())
}

pub fn get_host() -> String {
    STORE.with(|s| s.borrow().location.host())
}

pub fn get_hostname() -> String {
    STORE.with(|s| s.borrow().location.hostname().to_string())
}

pub fn get_port() -> String {
    STORE.with(|s| s.borrow().location.port().to_string())
}

pub fn get_protocol() -> String {
    STORE.with(|s| s.borrow().location.protocol().to_string())
}

pub fn get_origin() -> String {
    STORE.with(|s| s.borrow().location.origin())
}

// ---------------------------------------------------------------------------
// History getters
// ---------------------------------------------------------------------------

pub fn get_state() -> Option<String> {
    STORE.with(|s| s.borrow().history.state().map(|s| s.to_string()))
}

pub fn get_length() -> usize {
    STORE.with(|s| s.borrow().history.length())
}

// ---------------------------------------------------------------------------
// Event handler registration
// ---------------------------------------------------------------------------

pub fn register_popstate_handler(handler: fn(Option<String>)) {
    STORE.with(|s| s.borrow_mut().popstate_handlers.push(handler));
}

pub fn register_hashchange_handler(handler: fn(String, String)) {
    STORE.with(|s| s.borrow_mut().hashchange_handlers.push(handler));
}

fn fire_popstate(state: Option<String>) {
    STORE.with(|s| {
        let store = s.borrow();
        for handler in &store.popstate_handlers {
            handler(state.clone());
        }
    });
}

#[allow(dead_code)]
fn fire_hashchange(old_url: String, new_url: String) {
    STORE.with(|s| {
        let store = s.borrow();
        for handler in &store.hashchange_handlers {
            handler(old_url.clone(), new_url.clone());
        }
    });
}

/// Reset the history store (for testing or between app sessions).
pub fn reset() {
    STORE.with(|s| {
        *s.borrow_mut() = HistoryStore::new();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        reset();
        crate::state::clear_dirty();
    }

    #[test]
    fn push_state_updates_pathname() {
        setup();
        push_state(None, "", "/page1");
        assert_eq!(get_pathname(), "/page1");
        assert!(crate::state::is_dirty());
    }

    #[test]
    fn replace_state_keeps_length() {
        setup();
        push_state(None, "", "/a");
        assert_eq!(get_length(), 2);
        replace_state(Some(r#"{"x":1}"#), "", "/b");
        assert_eq!(get_length(), 2);
        assert_eq!(get_pathname(), "/b");
        assert_eq!(get_state(), Some(r#"{"x":1}"#.to_string()));
    }

    #[test]
    fn back_forward_navigate() {
        setup();
        push_state(None, "", "/a");
        push_state(None, "", "/b");
        back();
        assert_eq!(get_pathname(), "/a");
        forward();
        assert_eq!(get_pathname(), "/b");
    }

    #[test]
    fn go_jumps_correctly() {
        setup();
        push_state(None, "", "/a");
        push_state(None, "", "/b");
        push_state(None, "", "/c");
        go(-3);
        assert_eq!(get_pathname(), "/");
        go(2);
        assert_eq!(get_pathname(), "/b");
    }

    #[test]
    fn location_query_hash() {
        setup();
        push_state(None, "", "/search?q=rust#results");
        assert_eq!(get_pathname(), "/search");
        assert_eq!(get_search(), "?q=rust");
        assert_eq!(get_hash(), "#results");
    }

    #[test]
    fn popstate_handler_fires_on_back() {
        setup();
        use std::cell::Cell;
        thread_local! {
            static FIRED: Cell<bool> = const { Cell::new(false) };
        }
        fn handler(_state: Option<String>) {
            FIRED.with(|f| f.set(true));
        }
        register_popstate_handler(handler);
        push_state(Some("s1".into()), "", "/x");
        FIRED.with(|f| f.set(false));
        back();
        assert!(FIRED.with(|f| f.get()));
    }
}
