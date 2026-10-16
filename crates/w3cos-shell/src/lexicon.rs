//! Unlocked personal lexicon for the shell.
//!
//! The passphrase is stretched once at unlock and only the key stays in
//! memory. Secure sessions never reach this store. Disk sync is out of scope.

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use w3cos_ime::{
    Entry, Lexicon, derive_lexicon_key, learn, open_lexicon, rank_candidates, seal_with_key,
};

struct Store {
    memory_only: bool,
    unlocked: bool,
    unreadable: bool,
    key: Option<[u8; 32]>,
    salt: Option<[u8; 16]>,
    lexicon: Lexicon,
}

impl Store {
    fn new() -> Self {
        Self {
            memory_only: false,
            unlocked: false,
            unreadable: false,
            key: None,
            salt: None,
            lexicon: Lexicon::default(),
        }
    }
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::new());
}

fn with_store<R>(body: impl FnOnce(&mut Store) -> R) -> R {
    STORE.with(|slot| body(&mut slot.borrow_mut()))
}

pub(crate) fn unlock(passphrase: &str) {
    if passphrase.is_empty() {
        return;
    }
    with_store(|store| {
        if store.memory_only {
            store.unlocked = true;
            store.unreadable = false;
            return;
        }
        if let Some(path) = lexicon_path()
            && path.exists()
            && let Ok(bytes) = fs::read(&path)
        {
            if let Some((lexicon, key, salt)) = open_lexicon(&bytes, passphrase) {
                store.lexicon = lexicon;
                store.key = Some(key);
                store.salt = Some(salt);
                store.unlocked = true;
                store.unreadable = false;
            } else {
                store.unlocked = false;
                store.unreadable = true;
            }
            return;
        }
        let mut salt = [0u8; 16];
        if getrandom::getrandom(&mut salt).is_err() {
            return;
        }
        if let Some(key) = derive_lexicon_key(passphrase, &salt) {
            store.key = Some(key);
            store.salt = Some(salt);
            store.lexicon = Lexicon::default();
            store.unlocked = true;
            store.unreadable = false;
        }
    });
}

pub(crate) fn lock() {
    with_store(|store| {
        if store.memory_only {
            return;
        }
        store.unlocked = false;
        store.key = None;
        store.lexicon = Lexicon::default();
    });
}

pub(crate) fn unlocked() -> bool {
    with_store(|store| store.unlocked)
}

pub(crate) fn entries() -> Vec<Entry> {
    with_store(|store| {
        if !store.unlocked {
            return Vec::new();
        }
        store.lexicon.entries().to_vec()
    })
}

pub(crate) fn stealth_on() -> bool {
    with_store(|store| store.unlocked && store.lexicon.stealth)
}

pub(crate) fn app_hidden(app: &str) -> bool {
    with_store(|store| store.unlocked && store.lexicon.app_hidden(app))
}

pub(crate) fn ranked(spelling: &str) -> Option<Vec<String>> {
    with_store(|store| {
        if !store.unlocked {
            return None;
        }
        Some(rank_candidates(spelling, &store.lexicon))
    })
}

pub(crate) fn note_commit(
    scope: &str,
    secure: bool,
    spelling: &str,
    chosen: &str,
    skipped: &[String],
) {
    if secure {
        return;
    }
    with_store(|store| {
        if !store.unlocked || store.lexicon.app_hidden(scope) {
            return;
        }
        if learn(&mut store.lexicon, false, spelling, chosen, skipped) {
            persist(store);
        }
    });
}

/// Remember one picked handwriting or screen snippet.
///
/// Recognition, clear, and dismiss do not call this. Unpicked text is not
/// learned and is not sent to another device. A secure field never calls it.
pub(crate) fn note_ink_pick(scope: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    with_store(|store| {
        if !store.unlocked || store.lexicon.app_hidden(scope) {
            return;
        }
        if store.lexicon.accept_surface(text) {
            persist(store);
        }
    });
}

/// Same store as a handwriting pick. Only a chosen snippet is remembered.
pub(crate) fn note_screen_pick(scope: &str, text: &str) {
    note_ink_pick(scope, text);
}

pub(crate) fn note_reject(scope: &str, secure: bool, spelling: &str, text: &str) {
    if secure || text.is_empty() {
        return;
    }
    with_store(|store| {
        if !store.unlocked || store.lexicon.app_hidden(scope) {
            return;
        }
        store.lexicon.reject(spelling, text);
        persist(store);
    });
}

pub(crate) fn toggle_stealth() {
    with_store(|store| {
        if !store.unlocked {
            return;
        }
        let next = !store.lexicon.stealth;
        store.lexicon.set_stealth(next);
        persist(store);
    });
}

pub(crate) fn toggle_app(app: &str) {
    with_store(|store| {
        if !store.unlocked {
            return;
        }
        store.lexicon.toggle_app(app);
        persist(store);
    });
}

pub(crate) fn save_line(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    with_store(|store| {
        if !store.unlocked {
            return false;
        }
        let saved = if let Some((alias, target)) = line.split_once('=') {
            store.lexicon.upsert_alias(alias, target)
        } else {
            let mut parts = line.split_whitespace();
            let Some(text) = parts.next() else {
                return false;
            };
            let Some(spelling) = parts.next() else {
                return false;
            };
            store.lexicon.accept(spelling, text)
        };
        if saved {
            persist(store);
        }
        saved
    })
}

pub(crate) fn remove_line(line: &str) -> bool {
    let token = line.split([' ', '=']).next().unwrap_or("").trim();
    if token.is_empty() {
        return false;
    }
    with_store(|store| {
        if !store.unlocked {
            return false;
        }
        let removed = store.lexicon.remove_text(token);
        if removed {
            persist(store);
        }
        removed
    })
}

pub(crate) fn export_json() -> Option<String> {
    with_store(|store| {
        if !store.unlocked {
            return None;
        }
        Some(store.lexicon.export_json())
    })
}

pub(crate) fn clear() -> bool {
    with_store(|store| {
        if !store.unlocked {
            return false;
        }
        store.lexicon.clear_entries();
        persist(store);
        true
    })
}

pub(crate) fn seed_demo() {
    with_store(|store| {
        if !store.unlocked {
            return;
        }
        store.lexicon.clear_entries();
        for _ in 0..3 {
            store.lexicon.accept("nihao", "您好");
        }
        store.lexicon.upsert_alias("老王", "王明");
        persist(store);
    });
}

fn persist(store: &mut Store) {
    if store.memory_only || store.unreadable {
        return;
    }
    let (Some(key), Some(salt), Some(path)) = (store.key, store.salt, lexicon_path()) else {
        return;
    };
    let Some(bytes) = seal_with_key(&store.lexicon, &key, &salt) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(&tmp) {
        let _ = file.write_all(&bytes);
        let _ = file.sync_all();
        let _ = fs::rename(tmp, path);
    }
}

fn lexicon_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("W3COS_LEXICON_PATH") {
        if path.is_empty() || path == "-" {
            return None;
        }
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".w3cos/lexicon.w3p1"))
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    with_store(|store| {
        *store = Store {
            memory_only: true,
            unlocked: true,
            unreadable: false,
            key: Some([1; 32]),
            salt: Some([2; 16]),
            lexicon: Lexicon::default(),
        };
    });
}

#[cfg(test)]
pub(crate) fn freq_of(text: &str) -> Option<u32> {
    with_store(|store| {
        store
            .lexicon
            .entries()
            .iter()
            .find(|entry| entry.text == text)
            .map(|entry| entry.freq)
    })
}

#[cfg(test)]
pub(crate) fn neg_of(text: &str) -> Option<u32> {
    with_store(|store| {
        store
            .lexicon
            .entries()
            .iter()
            .find(|entry| entry.text == text)
            .map(|entry| entry.neg)
    })
}
