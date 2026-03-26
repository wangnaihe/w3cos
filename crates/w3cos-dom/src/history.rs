/// A single entry in the browser history stack.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub state: Option<String>,
    pub title: String,
    pub url: String,
}

/// W3C History API — manages the session history stack.
///
/// Mirrors the browser `window.history` interface: entries can be pushed,
/// replaced, and traversed with back/forward/go.
#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<HistoryEntry>,
    index: usize,
}

impl History {
    pub fn new() -> Self {
        Self {
            entries: vec![HistoryEntry {
                state: None,
                title: String::new(),
                url: "/".to_string(),
            }],
            index: 0,
        }
    }

    /// Push a new entry onto the history stack, discarding any forward entries.
    pub fn push_state(&mut self, state: Option<String>, title: &str, url: &str) {
        self.entries.truncate(self.index + 1);
        self.entries.push(HistoryEntry {
            state,
            title: title.to_string(),
            url: url.to_string(),
        });
        self.index = self.entries.len() - 1;
    }

    /// Replace the current entry without altering the stack length.
    pub fn replace_state(&mut self, state: Option<String>, title: &str, url: &str) {
        if let Some(entry) = self.entries.get_mut(self.index) {
            entry.state = state;
            entry.title = title.to_string();
            entry.url = url.to_string();
        }
    }

    /// Navigate one step back. Returns `true` if the index changed.
    pub fn back(&mut self) -> bool {
        if self.index > 0 {
            self.index -= 1;
            true
        } else {
            false
        }
    }

    /// Navigate one step forward. Returns `true` if the index changed.
    pub fn forward(&mut self) -> bool {
        if self.index + 1 < self.entries.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// Navigate by a relative offset. Returns `true` if the index changed.
    pub fn go(&mut self, delta: i32) -> bool {
        let target = self.index as i64 + delta as i64;
        if target >= 0 && (target as usize) < self.entries.len() {
            self.index = target as usize;
            true
        } else {
            false
        }
    }

    pub fn length(&self) -> usize {
        self.entries.len()
    }

    pub fn state(&self) -> Option<&str> {
        self.entries
            .get(self.index)
            .and_then(|e| e.state.as_deref())
    }

    pub fn current_url(&self) -> &str {
        self.entries
            .get(self.index)
            .map(|e| e.url.as_str())
            .unwrap_or("/")
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_history_has_root_entry() {
        let h = History::new();
        assert_eq!(h.length(), 1);
        assert_eq!(h.current_url(), "/");
        assert!(h.state().is_none());
    }

    #[test]
    fn push_state_adds_entry() {
        let mut h = History::new();
        h.push_state(Some(r#"{"page":1}"#.into()), "Page 1", "/page1");
        assert_eq!(h.length(), 2);
        assert_eq!(h.current_url(), "/page1");
        assert_eq!(h.state(), Some(r#"{"page":1}"#));
    }

    #[test]
    fn push_state_truncates_forward_entries() {
        let mut h = History::new();
        h.push_state(None, "", "/a");
        h.push_state(None, "", "/b");
        h.back();
        h.push_state(None, "", "/c");
        assert_eq!(h.length(), 3);
        assert_eq!(h.current_url(), "/c");
    }

    #[test]
    fn replace_state_keeps_length() {
        let mut h = History::new();
        h.push_state(None, "", "/a");
        h.replace_state(Some("replaced".into()), "New", "/b");
        assert_eq!(h.length(), 2);
        assert_eq!(h.current_url(), "/b");
        assert_eq!(h.state(), Some("replaced"));
    }

    #[test]
    fn back_and_forward() {
        let mut h = History::new();
        h.push_state(None, "", "/a");
        h.push_state(None, "", "/b");
        assert!(h.back());
        assert_eq!(h.current_url(), "/a");
        assert!(h.back());
        assert_eq!(h.current_url(), "/");
        assert!(!h.back());
        assert!(h.forward());
        assert_eq!(h.current_url(), "/a");
        assert!(h.forward());
        assert_eq!(h.current_url(), "/b");
        assert!(!h.forward());
    }

    #[test]
    fn go_positive_negative_zero() {
        let mut h = History::new();
        h.push_state(None, "", "/a");
        h.push_state(None, "", "/b");
        h.push_state(None, "", "/c");
        assert!(h.go(-2));
        assert_eq!(h.current_url(), "/a");
        assert!(h.go(2));
        assert_eq!(h.current_url(), "/c");
        assert!(h.go(0));
        assert!(!h.go(10));
        assert!(!h.go(-100));
    }
}
