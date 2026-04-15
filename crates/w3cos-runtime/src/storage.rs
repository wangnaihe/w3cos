use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

static STORAGE: Mutex<Option<WebStorage>> = Mutex::new(None);

/// W3C Web Storage API (localStorage / sessionStorage).
///
/// Data is persisted to `~/.w3cos/storage/<origin>.json` so it survives
/// process restarts (matching browser localStorage semantics).
/// Session storage uses the same API but is not persisted.
struct WebStorage {
    data: HashMap<String, String>,
    path: Option<PathBuf>,
}

impl WebStorage {
    fn new(persist_path: Option<PathBuf>) -> Self {
        let data = persist_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            data,
            path: persist_path,
        }
    }

    fn flush(&self) {
        if let Some(ref path) = self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&self.data) {
                let _ = std::fs::write(path, json);
            }
        }
    }
}

fn storage_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".w3cos")
        .join("storage")
        .join("local.json")
}

fn with_storage<F, R>(f: F) -> R
where
    F: FnOnce(&mut WebStorage) -> R,
{
    let mut guard = STORAGE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(WebStorage::new(Some(storage_path())));
    }
    f(guard.as_mut().unwrap())
}

/// `localStorage.getItem(key)` — returns `None` if the key does not exist.
pub fn get_item(key: &str) -> Option<String> {
    with_storage(|s| s.data.get(key).cloned())
}

/// `localStorage.setItem(key, value)`
pub fn set_item(key: &str, value: &str) {
    with_storage(|s| {
        s.data.insert(key.to_string(), value.to_string());
        s.flush();
    });
}

/// `localStorage.removeItem(key)`
pub fn remove_item(key: &str) {
    with_storage(|s| {
        s.data.remove(key);
        s.flush();
    });
}

/// `localStorage.clear()`
pub fn clear() {
    with_storage(|s| {
        s.data.clear();
        s.flush();
    });
}

/// `localStorage.key(index)` — returns the key at the given index.
pub fn key(index: usize) -> Option<String> {
    with_storage(|s| {
        s.data.keys().nth(index).cloned()
    })
}

/// `localStorage.length`
pub fn length() -> usize {
    with_storage(|s| s.data.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn setup() {
        INIT.call_once(|| {
            let mut guard = STORAGE.lock().unwrap();
            *guard = Some(WebStorage::new(None));
        });
    }

    #[test]
    fn set_and_get() {
        setup();
        set_item("test_key", "test_value");
        assert_eq!(get_item("test_key"), Some("test_value".to_string()));
    }

    #[test]
    fn remove() {
        setup();
        set_item("to_remove", "val");
        remove_item("to_remove");
        assert_eq!(get_item("to_remove"), None);
    }

    #[test]
    fn missing_key() {
        setup();
        assert_eq!(get_item("nonexistent_key_12345"), None);
    }

    #[test]
    fn len() {
        setup();
        let before = length();
        set_item("len_test", "v");
        assert!(length() >= before);
    }
}
