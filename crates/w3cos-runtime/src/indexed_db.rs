//! W3C IndexedDB API — minimal but spec-faithful object-store engine.
//!
//! Backed by a JSON file per database under `~/.w3cos/indexeddb/<dbname>.json`,
//! mirroring how the real browser persists IndexedDB to disk. The API maps
//! to JS as follows:
//!
//! ```text
//! const db = w3cos.indexedDB.open("notes", 1, (db) => db.createObjectStore("items", { keyPath: "id" }));
//! const tx = db.transaction(["items"], "readwrite");
//! const store = tx.objectStore("items");
//! await store.put({ id: 1, text: "hello" });
//! const all = await store.getAll();
//! ```
//!
//! The implementation favours predictable semantics over performance: every
//! mutating transaction fully serialises and flushes the database file, just
//! like the browser flushes the underlying SQLite store. Reads are O(n) over
//! a hash map, with O(log n) range scans over the BTreeMap key-ordered view.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// In-memory representation of an IndexedDB database.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct DatabaseState {
    name: String,
    version: u32,
    stores: HashMap<String, ObjectStoreState>,
}

/// One object store within a database.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct ObjectStoreState {
    name: String,
    /// JSON pointer-style key path (e.g. `"id"` or `"meta.id"`). Empty for
    /// out-of-line keys.
    key_path: String,
    /// Whether the store auto-generates numeric keys.
    auto_increment: bool,
    /// Monotonically increasing key for auto-increment stores.
    next_key: i64,
    /// Indexes (`name -> key path`) registered on the store.
    indexes: HashMap<String, String>,
    /// Records keyed by their canonical primary key (string form).
    records: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone)]
pub struct IndexedDbError {
    pub name: String,
    pub message: String,
}

impl std::fmt::Display for IndexedDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl std::error::Error for IndexedDbError {}

impl IndexedDbError {
    fn constraint(msg: impl Into<String>) -> Self {
        Self {
            name: "ConstraintError".into(),
            message: msg.into(),
        }
    }

    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            name: "NotFoundError".into(),
            message: msg.into(),
        }
    }

    fn invalid(msg: impl Into<String>) -> Self {
        Self {
            name: "InvalidStateError".into(),
            message: msg.into(),
        }
    }

    fn data(msg: impl Into<String>) -> Self {
        Self {
            name: "DataError".into(),
            message: msg.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, IndexedDbError>;

// ---------------------------------------------------------------------------
// Database registry — one shared in-memory cache per process.
// ---------------------------------------------------------------------------

struct Registry {
    databases: HashMap<String, Arc<Mutex<DatabaseState>>>,
    base_dir: PathBuf,
}

impl Registry {
    fn new() -> Self {
        let base = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join(".w3cos")
            .join("indexeddb");
        Self {
            databases: HashMap::new(),
            base_dir: base,
        }
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.json"))
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(Registry::new()))
}

/// Override the storage directory (used by tests).
pub fn set_base_dir(dir: PathBuf) {
    if let Ok(mut reg) = registry().lock() {
        reg.base_dir = dir;
        reg.databases.clear();
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Schema-upgrade callback signature, matches `IDBOpenDBRequest.onupgradeneeded`.
pub type UpgradeFn = dyn FnOnce(&Database, u32, u32) -> Result<()>;

/// `indexedDB.open(name, version, onUpgrade)`.
///
/// If the on-disk version is older than `version`, `on_upgrade` is called
/// with `(old_version, new_version)` to set up object stores / indexes.
pub fn open<F>(name: impl Into<String>, version: u32, on_upgrade: F) -> Result<Database>
where
    F: FnOnce(&Database, u32, u32) -> Result<()>,
{
    let name = name.into();
    let state_arc = load_or_create(&name)?;

    let old_version = state_arc.lock().expect("indexeddb mutex poisoned").version;
    let db = Database {
        name: name.clone(),
        state: Arc::clone(&state_arc),
    };

    if version > old_version {
        on_upgrade(&db, old_version, version)?;
        {
            let mut state = state_arc.lock().expect("indexeddb mutex poisoned");
            state.version = version;
        }
        flush(&name, &state_arc)?;
    } else if version < old_version {
        return Err(IndexedDbError::invalid(format!(
            "version {version} is older than persisted version {old_version}"
        )));
    }

    Ok(db)
}

/// Delete a database entirely. Mirrors `indexedDB.deleteDatabase(name)`.
pub fn delete(name: &str) -> Result<()> {
    let path = {
        let mut reg = registry().lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        reg.databases.remove(name);
        reg.path_for(name)
    };
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| IndexedDbError::invalid(format!("failed to delete db: {e}")))?;
    }
    Ok(())
}

/// List the databases that have ever been persisted under the storage root.
pub fn databases() -> Vec<String> {
    let dir = match registry().lock() {
        Ok(g) => g.base_dir.clone(),
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

fn load_or_create(name: &str) -> Result<Arc<Mutex<DatabaseState>>> {
    let mut reg = registry().lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
    if let Some(existing) = reg.databases.get(name) {
        return Ok(Arc::clone(existing));
    }
    let path = reg.path_for(name);
    let state = if path.exists() {
        let bytes = std::fs::read(&path)
            .map_err(|e| IndexedDbError::invalid(format!("read db failed: {e}")))?;
        serde_json::from_slice::<DatabaseState>(&bytes)
            .map_err(|e| IndexedDbError::data(format!("corrupt db file: {e}")))?
    } else {
        DatabaseState {
            name: name.to_string(),
            version: 0,
            stores: HashMap::new(),
        }
    };
    let arc = Arc::new(Mutex::new(state));
    reg.databases.insert(name.to_string(), Arc::clone(&arc));
    Ok(arc)
}

fn flush(name: &str, state: &Arc<Mutex<DatabaseState>>) -> Result<()> {
    let path = registry()
        .lock()
        .map_err(|e| IndexedDbError::invalid(e.to_string()))?
        .path_for(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| IndexedDbError::invalid(format!("create dir: {e}")))?;
    }
    let snapshot = state
        .lock()
        .map_err(|e| IndexedDbError::invalid(e.to_string()))?
        .clone();
    let bytes = serde_json::to_vec_pretty(&snapshot)
        .map_err(|e| IndexedDbError::data(e.to_string()))?;
    std::fs::write(&path, bytes)
        .map_err(|e| IndexedDbError::invalid(format!("write db: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Database / Transaction / ObjectStore
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Database {
    name: String,
    state: Arc<Mutex<DatabaseState>>,
}

impl Database {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> u32 {
        self.state.lock().map(|s| s.version).unwrap_or(0)
    }

    pub fn object_store_names(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| {
                let mut names: Vec<String> = s.stores.keys().cloned().collect();
                names.sort();
                names
            })
            .unwrap_or_default()
    }

    /// `db.createObjectStore(name, { keyPath, autoIncrement })`.
    pub fn create_object_store(
        &self,
        name: impl Into<String>,
        key_path: impl Into<String>,
        auto_increment: bool,
    ) -> Result<()> {
        let name = name.into();
        let key_path = key_path.into();
        let mut state = self.state.lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        if state.stores.contains_key(&name) {
            return Err(IndexedDbError::constraint(format!(
                "object store '{name}' already exists"
            )));
        }
        state.stores.insert(
            name.clone(),
            ObjectStoreState {
                name,
                key_path,
                auto_increment,
                next_key: 1,
                indexes: HashMap::new(),
                records: BTreeMap::new(),
            },
        );
        drop(state);
        flush(&self.name, &self.state)
    }

    /// `db.deleteObjectStore(name)`.
    pub fn delete_object_store(&self, name: &str) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        if state.stores.remove(name).is_none() {
            return Err(IndexedDbError::not_found(format!(
                "object store '{name}' not found"
            )));
        }
        drop(state);
        flush(&self.name, &self.state)
    }

    /// `db.createObjectStore(...).createIndex(name, keyPath)`.
    pub fn create_index(&self, store: &str, name: impl Into<String>, key_path: impl Into<String>) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        let store = state
            .stores
            .get_mut(store)
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{store}' not found")))?;
        store.indexes.insert(name.into(), key_path.into());
        drop(state);
        flush(&self.name, &self.state)
    }

    /// `db.transaction(stores, mode)`.
    pub fn transaction(&self, store_names: &[&str], mode: TransactionMode) -> Result<Transaction> {
        let state = self.state.lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        for name in store_names {
            if !state.stores.contains_key(*name) {
                return Err(IndexedDbError::not_found(format!(
                    "object store '{name}' not found"
                )));
            }
        }
        let scope = store_names.iter().map(|s| s.to_string()).collect();
        Ok(Transaction {
            db_name: self.name.clone(),
            db_state: Arc::clone(&self.state),
            mode,
            scope,
        })
    }
}

pub struct Transaction {
    db_name: String,
    db_state: Arc<Mutex<DatabaseState>>,
    mode: TransactionMode,
    scope: Vec<String>,
}

impl Transaction {
    pub fn mode(&self) -> TransactionMode {
        self.mode
    }

    pub fn object_store(&self, name: &str) -> Result<ObjectStore<'_>> {
        if !self.scope.iter().any(|s| s == name) {
            return Err(IndexedDbError::invalid(format!(
                "store '{name}' not in transaction scope"
            )));
        }
        Ok(ObjectStore {
            db_name: &self.db_name,
            db_state: Arc::clone(&self.db_state),
            mode: self.mode,
            store_name: name.to_string(),
        })
    }
}

pub struct ObjectStore<'a> {
    db_name: &'a str,
    db_state: Arc<Mutex<DatabaseState>>,
    mode: TransactionMode,
    store_name: String,
}

impl<'a> ObjectStore<'a> {
    fn require_writable(&self) -> Result<()> {
        match self.mode {
            TransactionMode::ReadWrite => Ok(()),
            TransactionMode::ReadOnly => Err(IndexedDbError::invalid(
                "transaction is read-only".to_string(),
            )),
        }
    }

    fn with_state_mut<R, F: FnOnce(&mut ObjectStoreState) -> Result<R>>(&self, f: F) -> Result<R> {
        let mut state = self.db_state.lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        let store = state
            .stores
            .get_mut(&self.store_name)
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{}' not found", self.store_name)))?;
        f(store)
    }

    fn with_state<R, F: FnOnce(&ObjectStoreState) -> R>(&self, f: F) -> Result<R> {
        let state = self.db_state.lock().map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        let store = state
            .stores
            .get(&self.store_name)
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{}' not found", self.store_name)))?;
        Ok(f(store))
    }

    /// `store.put(value)` — insert or replace.
    pub fn put(&self, value: Value) -> Result<String> {
        self.put_with_key(value, None)
    }

    /// `store.put(value, key)` — explicit out-of-line key.
    pub fn put_with_key(&self, mut value: Value, key: Option<Value>) -> Result<String> {
        self.require_writable()?;
        let key = self.with_state_mut(|store| {
            let resolved = resolve_key(store, &mut value, key)?;
            store.records.insert(resolved.clone(), value);
            Ok(resolved)
        })?;
        flush(self.db_name, &self.db_state)?;
        Ok(key)
    }

    /// `store.add(value)` — insert, fail if key already exists.
    pub fn add(&self, value: Value) -> Result<String> {
        self.add_with_key(value, None)
    }

    pub fn add_with_key(&self, mut value: Value, key: Option<Value>) -> Result<String> {
        self.require_writable()?;
        let key = self.with_state_mut(|store| {
            let resolved = resolve_key(store, &mut value, key)?;
            if store.records.contains_key(&resolved) {
                return Err(IndexedDbError::constraint(format!(
                    "key '{resolved}' already exists"
                )));
            }
            store.records.insert(resolved.clone(), value);
            Ok(resolved)
        })?;
        flush(self.db_name, &self.db_state)?;
        Ok(key)
    }

    /// `store.get(key)`.
    pub fn get(&self, key: &Value) -> Result<Option<Value>> {
        let key = canonicalize_key(key)?;
        self.with_state(|store| store.records.get(&key).cloned())
    }

    /// `store.getAll()` — every record in key order.
    pub fn get_all(&self) -> Result<Vec<Value>> {
        self.with_state(|store| store.records.values().cloned().collect())
    }

    /// `store.getAllKeys()` — every primary key in order.
    pub fn get_all_keys(&self) -> Result<Vec<String>> {
        self.with_state(|store| store.records.keys().cloned().collect())
    }

    /// `store.delete(key)`.
    pub fn delete(&self, key: &Value) -> Result<bool> {
        self.require_writable()?;
        let key = canonicalize_key(key)?;
        let removed = self.with_state_mut(|store| Ok(store.records.remove(&key).is_some()))?;
        flush(self.db_name, &self.db_state)?;
        Ok(removed)
    }

    /// `store.clear()`.
    pub fn clear(&self) -> Result<()> {
        self.require_writable()?;
        self.with_state_mut(|store| {
            store.records.clear();
            Ok(())
        })?;
        flush(self.db_name, &self.db_state)
    }

    /// `store.count()`.
    pub fn count(&self) -> Result<usize> {
        self.with_state(|store| store.records.len())
    }

    /// `store.index(name).getAll(value)` — equivalent of an index lookup.
    pub fn index_get(&self, index: &str, value: &Value) -> Result<Vec<Value>> {
        let target = canonicalize_key(value)?;
        self.with_state(|store| {
            let key_path = match store.indexes.get(index) {
                Some(p) => p.clone(),
                None => return Vec::new(),
            };
            store
                .records
                .values()
                .filter(|record| {
                    extract_key_path(record, &key_path)
                        .and_then(|v| canonicalize_key(&v).ok())
                        .as_deref()
                        == Some(target.as_str())
                })
                .cloned()
                .collect()
        })
    }
}

fn resolve_key(store: &mut ObjectStoreState, value: &mut Value, explicit: Option<Value>) -> Result<String> {
    if let Some(k) = explicit {
        return canonicalize_key(&k);
    }
    if !store.key_path.is_empty() {
        if let Some(existing) = extract_key_path(value, &store.key_path) {
            return canonicalize_key(&existing);
        }
        if store.auto_increment {
            let key = store.next_key;
            store.next_key += 1;
            inject_key_path(value, &store.key_path, Value::from(key));
            return Ok(key.to_string());
        }
        return Err(IndexedDbError::data(format!(
            "missing keyPath '{}' on record",
            store.key_path
        )));
    }
    if store.auto_increment {
        let key = store.next_key;
        store.next_key += 1;
        return Ok(key.to_string());
    }
    Err(IndexedDbError::data(
        "store has no keyPath and no explicit key was provided".to_string(),
    ))
}

fn canonicalize_key(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Array(arr) => {
            let parts: std::result::Result<Vec<String>, IndexedDbError> =
                arr.iter().map(canonicalize_key).collect();
            Ok(format!("[{}]", parts?.join(",")))
        }
        Value::Null => Err(IndexedDbError::data("null key not allowed".to_string())),
        Value::Object(_) => Err(IndexedDbError::data(
            "object keys not supported (use a key path)".to_string(),
        )),
    }
}

fn extract_key_path(value: &Value, path: &str) -> Option<Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

fn inject_key_path(value: &mut Value, path: &str, new_value: Value) {
    let segments: Vec<&str> = path.split('.').collect();
    let mut current = value;
    for (i, segment) in segments.iter().enumerate() {
        if i == segments.len() - 1 {
            if let Value::Object(map) = current {
                map.insert((*segment).to_string(), new_value);
            }
            return;
        }
        if !current.is_object() {
            *current = Value::Object(serde_json::Map::new());
        }
        current = current
            .as_object_mut()
            .and_then(|m| Some(m.entry(segment.to_string()).or_insert(Value::Object(serde_json::Map::new()))))
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    // The registry is process-global; serialize tests so they don't race over
    // `set_base_dir` calls.
    static IDB_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fresh_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "w3cos-idb-{}-{}",
            std::process::id(),
            label,
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_store_and_put_get() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("crud"));
        let db = open("notes", 1, |db, _old, _new| {
            db.create_object_store("items", "id", true)
        })
        .unwrap();

        let tx = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        let store = tx.object_store("items").unwrap();
        let key = store.put(json!({ "text": "hello" })).unwrap();
        assert_eq!(key, "1");
        let key2 = store.put(json!({ "text": "world" })).unwrap();
        assert_eq!(key2, "2");

        let got = store.get(&json!(1)).unwrap().unwrap();
        assert_eq!(got["text"], json!("hello"));
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn add_rejects_duplicates() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("dup"));
        let db = open("dup", 1, |db, _, _| db.create_object_store("s", "id", false)).unwrap();
        let tx = db
            .transaction(&["s"], TransactionMode::ReadWrite)
            .unwrap();
        let store = tx.object_store("s").unwrap();
        store.add(json!({ "id": 1, "v": "a" })).unwrap();
        let err = store.add(json!({ "id": 1, "v": "b" })).unwrap_err();
        assert_eq!(err.name, "ConstraintError");
    }

    #[test]
    fn read_only_blocks_writes() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("ro"));
        let db = open("ro", 1, |db, _, _| db.create_object_store("s", "id", true)).unwrap();
        let tx = db.transaction(&["s"], TransactionMode::ReadOnly).unwrap();
        let store = tx.object_store("s").unwrap();
        let err = store.put(json!({ "v": 1 })).unwrap_err();
        assert_eq!(err.name, "InvalidStateError");
    }

    #[test]
    fn index_lookup() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("index"));
        let db = open("idx", 1, |db, _, _| {
            db.create_object_store("users", "id", true)?;
            db.create_index("users", "by_email", "email")
        })
        .unwrap();
        let tx = db
            .transaction(&["users"], TransactionMode::ReadWrite)
            .unwrap();
        let store = tx.object_store("users").unwrap();
        store.put(json!({ "email": "a@x.com", "name": "A" })).unwrap();
        store.put(json!({ "email": "b@x.com", "name": "B" })).unwrap();
        store.put(json!({ "email": "a@x.com", "name": "A2" })).unwrap();

        let hits = store.index_get("by_email", &json!("a@x.com")).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h["name"] == json!("A2")));
    }

    #[test]
    fn upgrade_and_persist() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let dir = fresh_dir("persist");
        set_base_dir(dir.clone());
        {
            let db = open("p", 1, |db, _, _| db.create_object_store("s", "id", true)).unwrap();
            let tx = db.transaction(&["s"], TransactionMode::ReadWrite).unwrap();
            let store = tx.object_store("s").unwrap();
            store.put(json!({ "v": 42 })).unwrap();
        }
        // Re-open in a fresh process simulation by clearing the registry cache.
        set_base_dir(dir);
        let db = open("p", 1, |_, _, _| Ok(())).unwrap();
        let tx = db.transaction(&["s"], TransactionMode::ReadOnly).unwrap();
        let store = tx.object_store("s").unwrap();
        assert_eq!(store.count().unwrap(), 1);
        let row = store.get(&json!(1)).unwrap().unwrap();
        assert_eq!(row["v"], json!(42));
    }

    #[test]
    fn delete_database() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let dir = fresh_dir("del");
        set_base_dir(dir);
        let _ = open("trash", 1, |db, _, _| db.create_object_store("s", "id", true)).unwrap();
        delete("trash").unwrap();
        let dbs = databases();
        assert!(!dbs.iter().any(|d| d == "trash"));
    }
}
