//! IndexedDB object-store backend prototype.
//!
//! Backed by a SQLite file per database under
//! `~/.w3cos/indexeddb/<dbname>.sqlite3`. The JavaScript-visible W3C API lives
//! in `indexed_db_web`; this module is the hidden transactional engine.
//!
//! The current Rust prototype is used as follows:
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

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::indexed_db_sqlite;

#[cfg(test)]
pub(crate) static IDB_TEST_LOCK: Mutex<()> = Mutex::new(());

/// In-memory representation of an IndexedDB database.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub(crate) struct DatabaseState {
    pub(crate) name: String,
    pub(crate) version: u32,
    pub(crate) stores: HashMap<String, ObjectStoreState>,
}

/// One object store within a database.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub(crate) struct ObjectStoreState {
    pub(crate) name: String,
    /// JSON pointer-style key path (e.g. `"id"` or `"meta.id"`). Empty for
    /// out-of-line keys.
    pub(crate) key_path: String,
    /// Whether the store auto-generates numeric keys.
    pub(crate) auto_increment: bool,
    /// Monotonically increasing key for auto-increment stores.
    pub(crate) next_key: i64,
    /// Index definitions registered on the store.
    pub(crate) indexes: HashMap<String, IndexState>,
    /// Records keyed by their canonical primary key (string form).
    pub(crate) records: BTreeMap<String, StoredRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct StoredRecord {
    pub(crate) key: Value,
    pub(crate) value: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct IndexState {
    pub(crate) key_path: String,
    pub(crate) unique: bool,
    pub(crate) multi_entry: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone)]
pub struct KeyRange {
    lower: Option<String>,
    upper: Option<String>,
    lower_open: bool,
    upper_open: bool,
}

impl KeyRange {
    pub fn only(key: &Value) -> Result<Self> {
        let key = canonicalize_key(key)?;
        Ok(Self {
            lower: Some(key.clone()),
            upper: Some(key),
            lower_open: false,
            upper_open: false,
        })
    }

    pub fn lower_bound(key: &Value, open: bool) -> Result<Self> {
        Ok(Self {
            lower: Some(canonicalize_key(key)?),
            upper: None,
            lower_open: open,
            upper_open: false,
        })
    }

    pub fn upper_bound(key: &Value, open: bool) -> Result<Self> {
        Ok(Self {
            lower: None,
            upper: Some(canonicalize_key(key)?),
            lower_open: false,
            upper_open: open,
        })
    }

    pub fn bound(lower: &Value, upper: &Value, lower_open: bool, upper_open: bool) -> Result<Self> {
        let lower = canonicalize_key(lower)?;
        let upper = canonicalize_key(upper)?;
        if lower > upper || (lower == upper && (lower_open || upper_open)) {
            return Err(IndexedDbError::data("The key range is empty."));
        }
        Ok(Self {
            lower: Some(lower),
            upper: Some(upper),
            lower_open,
            upper_open,
        })
    }

    fn contains(&self, key: &str) -> bool {
        let after_lower = self
            .lower
            .as_ref()
            .is_none_or(|lower| key > lower.as_str() || (!self.lower_open && key == lower));
        let before_upper = self
            .upper
            .as_ref()
            .is_none_or(|upper| key < upper.as_str() || (!self.upper_open && key == upper));
        after_lower && before_upper
    }
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

    fn read_only(msg: impl Into<String>) -> Self {
        Self {
            name: "ReadOnlyError".into(),
            message: msg.into(),
        }
    }

    fn invalid_access(msg: impl Into<String>) -> Self {
        Self {
            name: "InvalidAccessError".into(),
            message: msg.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, IndexedDbError>;

pub fn compare_keys(left: &Value, right: &Value) -> Result<Ordering> {
    Ok(canonicalize_key(left)?.cmp(&canonicalize_key(right)?))
}

// ---------------------------------------------------------------------------
// Database registry — one shared in-memory cache per process.
// ---------------------------------------------------------------------------

struct Registry {
    databases: HashMap<String, Arc<Mutex<DatabaseState>>>,
    base_dir: PathBuf,
    quota_bytes: u64,
}

impl Registry {
    fn new() -> Self {
        let storage_scope = format!(
            "{}|{}",
            std::env::var("W3COS_APP_ID").unwrap_or_else(|_| "default-app".into()),
            std::env::var("W3COS_STORAGE_KEY").unwrap_or_else(|_| "local".into())
        );
        let base = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join(".w3cos")
            .join("indexeddb")
            .join(stable_identifier(&storage_scope));
        Self {
            databases: HashMap::new(),
            base_dir: base,
            quota_bytes: 256 * 1024 * 1024,
        }
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.base_dir
            .join(format!("{}.sqlite3", stable_identifier(name)))
    }
}

fn stable_identifier(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
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

/// Override the per-storage-scope quota. Embedders normally keep the default;
/// tests and platform policy may choose a smaller limit.
pub fn set_quota_bytes(bytes: u64) {
    if let Ok(mut reg) = registry().lock() {
        reg.quota_bytes = bytes;
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
        flush_schema_changes: true,
    };

    if version > old_version {
        let before_upgrade = state_arc
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?
            .clone();
        let upgrade_db = Database {
            name: name.clone(),
            state: Arc::clone(&state_arc),
            flush_schema_changes: false,
        };
        let upgrade_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            on_upgrade(&upgrade_db, old_version, version)
        }));
        let upgrade_error = match upgrade_result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some(IndexedDbError {
                name: "AbortError".into(),
                message: "The versionchange transaction was aborted by its event handler.".into(),
            }),
        };
        if let Some(error) = upgrade_error {
            *state_arc
                .lock()
                .map_err(|lock_error| IndexedDbError::invalid(lock_error.to_string()))? =
                before_upgrade;
            return Err(error);
        }
        {
            let mut state = state_arc.lock().expect("indexeddb mutex poisoned");
            state.version = version;
        }
        if let Err(error) = flush(&name, &state_arc) {
            *state_arc
                .lock()
                .map_err(|lock_error| IndexedDbError::invalid(lock_error.to_string()))? =
                before_upgrade;
            return Err(error);
        }
    } else if version < old_version {
        return Err(IndexedDbError {
            name: "VersionError".into(),
            message: format!("version {version} is older than persisted version {old_version}"),
        });
    }

    Ok(db)
}

/// Return the persisted/current version used when `indexedDB.open(name)`
/// omits its version argument. A new database reports version zero.
pub fn current_version(name: &str) -> Result<u32> {
    let state = load_or_create(name)?;
    state
        .lock()
        .map(|state| state.version)
        .map_err(|error| IndexedDbError::invalid(error.to_string()))
}

/// Delete a database entirely. Mirrors `indexedDB.deleteDatabase(name)`.
pub fn delete(name: &str) -> Result<()> {
    let path = {
        let mut reg = registry()
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        reg.databases.remove(name);
        reg.path_for(name)
    };
    for candidate in [
        path.clone(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() {
            std::fs::remove_file(&candidate)
                .map_err(|e| IndexedDbError::invalid(format!("failed to delete db: {e}")))?;
        }
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
            if path.extension().and_then(|s| s.to_str()) == Some("sqlite3") {
                if let Ok(Some(state)) = indexed_db_sqlite::read_state(&path) {
                    out.push(state.name);
                }
            }
        }
    }
    out
}

fn load_or_create(name: &str) -> Result<Arc<Mutex<DatabaseState>>> {
    let mut reg = registry()
        .lock()
        .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
    if let Some(existing) = reg.databases.get(name) {
        return Ok(Arc::clone(existing));
    }
    let path = reg.path_for(name);
    let state = if path.exists() {
        indexed_db_sqlite::read_state(&path)?
            .ok_or_else(|| IndexedDbError::data("database has no state record"))?
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
    let (path, quota_bytes) = {
        let registry = registry()
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        (registry.path_for(name), registry.quota_bytes)
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| IndexedDbError::invalid(format!("create dir: {e}")))?;
    }
    let snapshot = state
        .lock()
        .map_err(|e| IndexedDbError::invalid(e.to_string()))?
        .clone();
    let estimated_bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| IndexedDbError::data(error.to_string()))?
        .len() as u64
        * 2
        + 64 * 1024;
    if estimated_bytes > quota_bytes {
        return Err(IndexedDbError {
            name: "QuotaExceededError".into(),
            message: format!(
                "IndexedDB storage requires approximately {estimated_bytes} bytes; quota is {quota_bytes} bytes."
            ),
        });
    }
    indexed_db_sqlite::write_state(&path, &snapshot)
}

// ---------------------------------------------------------------------------
// Database / Transaction / ObjectStore
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Database {
    name: String,
    state: Arc<Mutex<DatabaseState>>,
    flush_schema_changes: bool,
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

    pub fn object_store_definition(&self, name: &str) -> Result<(String, bool)> {
        let state = self
            .state
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
        state
            .stores
            .get(name)
            .map(|store| (store.key_path.clone(), store.auto_increment))
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{name}' not found")))
    }

    pub fn index_names(&self, store_name: &str) -> Result<Vec<String>> {
        let state = self
            .state
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
        let store = state
            .stores
            .get(store_name)
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{store_name}' not found")))?;
        let mut names = store.indexes.keys().cloned().collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }

    /// `db.createObjectStore(name, { keyPath, autoIncrement })`.
    pub fn create_object_store(
        &self,
        name: impl Into<String>,
        key_path: impl Into<String>,
        auto_increment: bool,
    ) -> Result<()> {
        let name = name.into();
        let key_path = normalize_key_path_storage(key_path.into());
        if auto_increment
            && key_path_parts(&key_path)
                .is_some_and(|parts| parts.len() != 1 || parts[0].is_empty())
        {
            return Err(IndexedDbError::invalid_access(
                "autoIncrement cannot be used with a compound or empty key path.",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
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
        if self.flush_schema_changes {
            flush(&self.name, &self.state)
        } else {
            Ok(())
        }
    }

    /// `db.deleteObjectStore(name)`.
    pub fn delete_object_store(&self, name: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        if state.stores.remove(name).is_none() {
            return Err(IndexedDbError::not_found(format!(
                "object store '{name}' not found"
            )));
        }
        drop(state);
        if self.flush_schema_changes {
            flush(&self.name, &self.state)
        } else {
            Ok(())
        }
    }

    /// `db.createObjectStore(...).createIndex(name, keyPath)`.
    pub fn create_index(
        &self,
        store: &str,
        name: impl Into<String>,
        key_path: impl Into<String>,
    ) -> Result<()> {
        self.create_index_with_options(store, name, key_path, false, false)
    }

    pub fn create_index_with_options(
        &self,
        store: &str,
        name: impl Into<String>,
        key_path: impl Into<String>,
        unique: bool,
        multi_entry: bool,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        let store = state
            .stores
            .get_mut(store)
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{store}' not found")))?;
        let name = name.into();
        if store.indexes.contains_key(&name) {
            return Err(IndexedDbError::constraint(format!(
                "index '{name}' already exists"
            )));
        }
        let key_path = normalize_key_path_storage(key_path.into());
        if key_path_parts(&key_path).is_none() {
            return Err(IndexedDbError::data("An index requires a key path."));
        }
        if multi_entry && key_path_parts(&key_path).is_some_and(|parts| parts.len() > 1) {
            return Err(IndexedDbError::invalid_access(
                "multiEntry cannot be used with a compound key path.",
            ));
        }
        let definition = IndexState {
            key_path,
            unique,
            multi_entry,
        };
        if unique {
            let mut seen = std::collections::HashSet::new();
            for record in store.records.values() {
                for key in index_keys(&definition, &record.value) {
                    let canonical = canonicalize_key(&key)?;
                    if !seen.insert(canonical) {
                        return Err(IndexedDbError::constraint(
                            "Existing records violate the unique index constraint.",
                        ));
                    }
                }
            }
        }
        store.indexes.insert(name, definition);
        drop(state);
        if self.flush_schema_changes {
            flush(&self.name, &self.state)
        } else {
            Ok(())
        }
    }

    pub fn delete_index(&self, store_name: &str, index_name: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
        let store = state
            .stores
            .get_mut(store_name)
            .ok_or_else(|| IndexedDbError::not_found(format!("store '{store_name}' not found")))?;
        if store.indexes.remove(index_name).is_none() {
            return Err(IndexedDbError::not_found(format!(
                "index '{index_name}' not found"
            )));
        }
        drop(state);
        if self.flush_schema_changes {
            flush(&self.name, &self.state)
        } else {
            Ok(())
        }
    }

    /// `db.transaction(stores, mode)`.
    pub fn transaction(&self, store_names: &[&str], mode: TransactionMode) -> Result<Transaction> {
        if store_names.is_empty() {
            return Err(IndexedDbError::invalid_access(
                "A transaction scope must contain at least one object store.",
            ));
        }
        let state = self
            .state
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
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
            working_state: Arc::new(Mutex::new(None)),
            finished: Arc::new(Mutex::new(false)),
        })
    }
}

#[derive(Clone)]
pub struct Transaction {
    db_name: String,
    db_state: Arc<Mutex<DatabaseState>>,
    mode: TransactionMode,
    scope: Vec<String>,
    /// Lazily captured snapshot. Lazy start lets the Web scheduler create a
    /// transaction handle before an earlier overlapping transaction commits.
    working_state: Arc<Mutex<Option<DatabaseState>>>,
    finished: Arc<Mutex<bool>>,
}

impl Transaction {
    pub fn mode(&self) -> TransactionMode {
        self.mode
    }

    pub fn object_store(&self, name: &str) -> Result<ObjectStore> {
        self.require_active()?;
        if !self.scope.iter().any(|s| s == name) {
            return Err(IndexedDbError::invalid(format!(
                "store '{name}' not in transaction scope"
            )));
        }
        Ok(ObjectStore {
            transaction: self.clone(),
            store_name: name.to_string(),
        })
    }

    pub fn commit(&self) -> Result<()> {
        self.require_active()?;
        if self.mode == TransactionMode::ReadWrite {
            self.ensure_working_state()?;
            let working = self
                .working_state
                .lock()
                .map_err(|error| IndexedDbError::invalid(error.to_string()))?
                .clone()
                .expect("transaction snapshot initialized");
            {
                let mut current = self
                    .db_state
                    .lock()
                    .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
                for name in &self.scope {
                    match working.stores.get(name) {
                        Some(store) => {
                            current.stores.insert(name.clone(), store.clone());
                        }
                        None => {
                            current.stores.remove(name);
                        }
                    }
                }
            }
            flush(&self.db_name, &self.db_state)?;
        }
        *self
            .finished
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))? = true;
        Ok(())
    }

    pub fn abort(&self) -> Result<()> {
        self.require_active()?;
        *self
            .working_state
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))? = None;
        *self
            .finished
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))? = true;
        Ok(())
    }

    fn require_active(&self) -> Result<()> {
        let finished = *self
            .finished
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
        if finished {
            Err(IndexedDbError {
                name: "TransactionInactiveError".into(),
                message: "The transaction has finished.".into(),
            })
        } else {
            Ok(())
        }
    }

    fn ensure_working_state(&self) -> Result<()> {
        self.require_active()?;
        let mut working = self
            .working_state
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
        if working.is_none() {
            *working = Some(
                self.db_state
                    .lock()
                    .map_err(|error| IndexedDbError::invalid(error.to_string()))?
                    .clone(),
            );
        }
        Ok(())
    }
}

pub struct ObjectStore {
    transaction: Transaction,
    store_name: String,
}

impl ObjectStore {
    fn require_writable(&self) -> Result<()> {
        self.transaction.require_active()?;
        match self.transaction.mode {
            TransactionMode::ReadWrite => Ok(()),
            TransactionMode::ReadOnly => Err(IndexedDbError::read_only(
                "transaction is read-only".to_string(),
            )),
        }
    }

    fn with_state_mut<R, F: FnOnce(&mut ObjectStoreState) -> Result<R>>(&self, f: F) -> Result<R> {
        self.transaction.ensure_working_state()?;
        let mut working = self
            .transaction
            .working_state
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        let state = working.as_mut().expect("transaction snapshot initialized");
        let store = state.stores.get_mut(&self.store_name).ok_or_else(|| {
            IndexedDbError::not_found(format!("store '{}' not found", self.store_name))
        })?;
        f(store)
    }

    fn with_state<R, F: FnOnce(&ObjectStoreState) -> R>(&self, f: F) -> Result<R> {
        self.transaction.ensure_working_state()?;
        let working = self
            .transaction
            .working_state
            .lock()
            .map_err(|e| IndexedDbError::invalid(e.to_string()))?;
        let state = working.as_ref().expect("transaction snapshot initialized");
        let store = state.stores.get(&self.store_name).ok_or_else(|| {
            IndexedDbError::not_found(format!("store '{}' not found", self.store_name))
        })?;
        Ok(f(store))
    }

    pub fn definition(&self) -> Result<(String, bool, Vec<String>)> {
        let state = self
            .transaction
            .db_state
            .lock()
            .map_err(|error| IndexedDbError::invalid(error.to_string()))?;
        let store = state.stores.get(&self.store_name).ok_or_else(|| {
            IndexedDbError::not_found(format!("store '{}' not found", self.store_name))
        })?;
        let mut indexes = store.indexes.keys().cloned().collect::<Vec<_>>();
        indexes.sort();
        Ok((store.key_path.clone(), store.auto_increment, indexes))
    }

    /// `store.put(value)` — insert or replace.
    pub fn put(&self, value: Value) -> Result<Value> {
        self.put_with_key(value, None)
    }

    /// `store.put(value, key)` — explicit out-of-line key.
    pub fn put_with_key(&self, mut value: Value, key: Option<Value>) -> Result<Value> {
        self.require_writable()?;
        let key = self.with_state_mut(|store| {
            let resolved = resolve_key(store, &mut value, key)?;
            validate_unique_indexes(store, &resolved.canonical, &value)?;
            store.records.insert(
                resolved.canonical,
                StoredRecord {
                    key: resolved.value.clone(),
                    value,
                },
            );
            Ok(resolved.value)
        })?;
        Ok(key)
    }

    /// `store.add(value)` — insert, fail if key already exists.
    pub fn add(&self, value: Value) -> Result<Value> {
        self.add_with_key(value, None)
    }

    pub fn add_with_key(&self, mut value: Value, key: Option<Value>) -> Result<Value> {
        self.require_writable()?;
        let key = self.with_state_mut(|store| {
            let resolved = resolve_key(store, &mut value, key)?;
            if store.records.contains_key(&resolved.canonical) {
                return Err(IndexedDbError::constraint(format!(
                    "key '{}' already exists",
                    resolved.canonical
                )));
            }
            validate_unique_indexes(store, &resolved.canonical, &value)?;
            store.records.insert(
                resolved.canonical,
                StoredRecord {
                    key: resolved.value.clone(),
                    value,
                },
            );
            Ok(resolved.value)
        })?;
        Ok(key)
    }

    /// `store.get(key)`.
    pub fn get(&self, key: &Value) -> Result<Option<Value>> {
        let key = canonicalize_key(key)?;
        self.with_state(|store| store.records.get(&key).map(|record| record.value.clone()))
    }

    /// `store.getAll()` — every record in key order.
    pub fn get_all(&self) -> Result<Vec<Value>> {
        self.with_state(|store| {
            store
                .records
                .values()
                .map(|record| record.value.clone())
                .collect()
        })
    }

    pub fn get_all_range(
        &self,
        range: Option<&KeyRange>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>> {
        self.with_state(|store| {
            store
                .records
                .iter()
                .filter(|(key, _)| range.is_none_or(|range| range.contains(key)))
                .map(|(_, record)| record.value.clone())
                .take(limit.unwrap_or(usize::MAX))
                .collect()
        })
    }

    /// `store.getAllKeys()` — every primary key in order.
    pub fn get_all_keys(&self) -> Result<Vec<Value>> {
        self.with_state(|store| {
            store
                .records
                .values()
                .map(|record| record.key.clone())
                .collect()
        })
    }

    pub fn get_all_keys_range(
        &self,
        range: Option<&KeyRange>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>> {
        self.with_state(|store| {
            store
                .records
                .iter()
                .filter(|(key, _)| range.is_none_or(|range| range.contains(key)))
                .map(|(_, record)| record.key.clone())
                .take(limit.unwrap_or(usize::MAX))
                .collect()
        })
    }

    pub fn scan_range(&self, range: Option<&KeyRange>) -> Result<Vec<(Value, Value)>> {
        self.with_state(|store| {
            store
                .records
                .iter()
                .filter(|(key, _)| range.is_none_or(|range| range.contains(key)))
                .map(|(_, record)| (record.key.clone(), record.value.clone()))
                .collect()
        })
    }

    /// `store.delete(key)`.
    pub fn delete(&self, key: &Value) -> Result<bool> {
        self.require_writable()?;
        let key = canonicalize_key(key)?;
        let removed = self.with_state_mut(|store| Ok(store.records.remove(&key).is_some()))?;
        Ok(removed)
    }

    pub fn delete_range(&self, range: &KeyRange) -> Result<usize> {
        self.require_writable()?;
        self.with_state_mut(|store| {
            let keys = store
                .records
                .keys()
                .filter(|key| range.contains(key))
                .cloned()
                .collect::<Vec<_>>();
            let count = keys.len();
            for key in keys {
                store.records.remove(&key);
            }
            Ok(count)
        })
    }

    /// `store.clear()`.
    pub fn clear(&self) -> Result<()> {
        self.require_writable()?;
        self.with_state_mut(|store| {
            store.records.clear();
            Ok(())
        })
    }

    /// `store.count()`.
    pub fn count(&self) -> Result<usize> {
        self.with_state(|store| store.records.len())
    }

    pub fn count_range(&self, range: Option<&KeyRange>) -> Result<usize> {
        self.with_state(|store| {
            store
                .records
                .keys()
                .filter(|key| range.is_none_or(|range| range.contains(key)))
                .count()
        })
    }

    /// `store.index(name).getAll(value)` — equivalent of an index lookup.
    pub fn index_get(&self, index: &str, value: &Value) -> Result<Vec<Value>> {
        let range = KeyRange::only(value)?;
        Ok(self
            .index_scan(index, Some(&range))?
            .into_iter()
            .map(|(_, _, value)| value)
            .collect())
    }

    pub fn index_scan(
        &self,
        index: &str,
        range: Option<&KeyRange>,
    ) -> Result<Vec<(Value, Value, Value)>> {
        self.with_state(|store| {
            let definition = store
                .indexes
                .get(index)
                .ok_or_else(|| IndexedDbError::not_found(format!("index '{index}' not found")))?;
            let mut entries = Vec::new();
            for record in store.records.values() {
                for index_key in index_keys(definition, &record.value) {
                    let canonical = canonicalize_key(&index_key)?;
                    if range.is_none_or(|range| range.contains(&canonical)) {
                        entries.push((
                            canonical,
                            index_key,
                            record.key.clone(),
                            record.value.clone(),
                        ));
                    }
                }
            }
            entries.sort_by(|left, right| {
                left.0.cmp(&right.0).then_with(|| {
                    canonicalize_key(&left.2)
                        .unwrap_or_default()
                        .cmp(&canonicalize_key(&right.2).unwrap_or_default())
                })
            });
            Ok(entries
                .into_iter()
                .map(|(_, index_key, primary_key, value)| (index_key, primary_key, value))
                .collect())
        })?
    }

    pub fn index_definition(&self, index: &str) -> Result<(String, bool, bool)> {
        self.with_state(|store| {
            store
                .indexes
                .get(index)
                .map(|definition| {
                    (
                        definition.key_path.clone(),
                        definition.unique,
                        definition.multi_entry,
                    )
                })
                .ok_or_else(|| IndexedDbError::not_found(format!("index '{index}' not found")))
        })?
    }
}

pub(crate) fn index_keys(index: &IndexState, value: &Value) -> Vec<Value> {
    let Some(value) = extract_key_path(value, &index.key_path) else {
        return Vec::new();
    };
    if index.multi_entry {
        if let Value::Array(values) = value {
            let mut seen = std::collections::HashSet::new();
            return values
                .into_iter()
                .filter(|value| {
                    canonicalize_key(value)
                        .ok()
                        .is_some_and(|key| seen.insert(key))
                })
                .collect();
        }
    }
    canonicalize_key(&value)
        .ok()
        .map_or_else(Vec::new, |_| vec![value])
}

fn validate_unique_indexes(
    store: &ObjectStoreState,
    primary_key: &str,
    value: &Value,
) -> Result<()> {
    for index in store.indexes.values().filter(|index| index.unique) {
        let candidate_keys = index_keys(index, value)
            .into_iter()
            .filter_map(|key| canonicalize_key(&key).ok())
            .collect::<Vec<_>>();
        for (stored_primary_key, record) in &store.records {
            if stored_primary_key == primary_key {
                continue;
            }
            let conflict = index_keys(index, &record.value).into_iter().any(|key| {
                canonicalize_key(&key)
                    .ok()
                    .is_some_and(|key| candidate_keys.contains(&key))
            });
            if conflict {
                return Err(IndexedDbError::constraint(
                    "A unique index already contains this key.",
                ));
            }
        }
    }
    Ok(())
}

struct ResolvedKey {
    canonical: String,
    value: Value,
}

fn resolve_key(
    store: &mut ObjectStoreState,
    value: &mut Value,
    explicit: Option<Value>,
) -> Result<ResolvedKey> {
    if let Some(k) = explicit {
        return Ok(ResolvedKey {
            canonical: canonicalize_key(&k)?,
            value: k,
        });
    }
    if !store.key_path.is_empty() {
        if let Some(existing) = extract_key_path(value, &store.key_path) {
            return Ok(ResolvedKey {
                canonical: canonicalize_key(&existing)?,
                value: existing,
            });
        }
        if store.auto_increment {
            let key = store.next_key;
            store.next_key += 1;
            inject_key_path(value, &store.key_path, Value::from(key));
            let value = Value::from(key);
            return Ok(ResolvedKey {
                canonical: canonicalize_key(&value)?,
                value,
            });
        }
        return Err(IndexedDbError::data(format!(
            "missing keyPath '{}' on record",
            store.key_path
        )));
    }
    if store.auto_increment {
        let key = store.next_key;
        store.next_key += 1;
        let value = Value::from(key);
        return Ok(ResolvedKey {
            canonical: canonicalize_key(&value)?,
            value,
        });
    }
    Err(IndexedDbError::data(
        "store has no keyPath and no explicit key was provided".to_string(),
    ))
}

pub(crate) fn canonicalize_key(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(format!("3:{}", encode_utf16(value))),
        Value::Number(value) => {
            let number = value
                .as_f64()
                .ok_or_else(|| IndexedDbError::data("invalid numeric key"))?;
            if !number.is_finite() {
                return Err(IndexedDbError::data("non-finite key not allowed"));
            }
            let number = if number == 0.0 { 0.0 } else { number };
            let bits = number.to_bits();
            let sortable = if bits >> 63 == 1 {
                !bits
            } else {
                bits ^ (1_u64 << 63)
            };
            Ok(format!("1:{sortable:016x}"))
        }
        Value::Bool(_) => Err(IndexedDbError::data("boolean key not allowed")),
        Value::Array(arr) => {
            let mut encoded = String::from("5:");
            for part in arr {
                let part = canonicalize_key(part)?;
                encoded.push_str(&format!("{:08x}{part}", part.len()));
            }
            Ok(encoded)
        }
        Value::Null => Err(IndexedDbError::data("null key not allowed".to_string())),
        Value::Object(object) => {
            if let Some(bytes) = object
                .get("\u{1f}w3cos-idb-binary")
                .and_then(Value::as_array)
            {
                let mut encoded = String::from("4:");
                for byte in bytes {
                    let byte = byte
                        .as_u64()
                        .filter(|byte| *byte <= u8::MAX as u64)
                        .ok_or_else(|| IndexedDbError::data("invalid binary key"))?;
                    encoded.push_str(&format!("{byte:02x}"));
                }
                return Ok(encoded);
            }
            object
                .get("\u{1f}w3cos-idb-date")
                .and_then(Value::as_f64)
                .filter(|milliseconds| milliseconds.is_finite())
                .map(|milliseconds| {
                    let bits = (if milliseconds == 0.0 {
                        0.0
                    } else {
                        milliseconds
                    })
                    .to_bits();
                    let sortable = if bits >> 63 == 1 {
                        !bits
                    } else {
                        bits ^ (1_u64 << 63)
                    };
                    format!("2:{sortable:016x}")
                })
                .ok_or_else(|| {
                    IndexedDbError::data("object keys not supported (use a key path)".to_string())
                })
        }
    }
}

const KEY_PATH_PREFIX: &str = "\u{1f}w3cos-key-path:";
const NO_KEY_PATH: &str = "\u{1f}w3cos-key-path:none";
const EMPTY_KEY_PATH: &str = "\u{1f}w3cos-key-path:single:";
const COMPOUND_KEY_PATH: &str = "\u{1f}w3cos-key-path:compound:";

pub(crate) fn encode_key_path(parts: Option<&[String]>) -> String {
    match parts {
        None => NO_KEY_PATH.into(),
        Some([single]) if single.is_empty() => EMPTY_KEY_PATH.into(),
        Some([single]) => single.clone(),
        Some(parts) => format!(
            "{COMPOUND_KEY_PATH}{}",
            serde_json::to_string(parts).expect("key paths are serializable")
        ),
    }
}

pub(crate) fn key_path_parts(path: &str) -> Option<Vec<String>> {
    if path.is_empty() || path == NO_KEY_PATH {
        None
    } else if path == EMPTY_KEY_PATH {
        Some(vec![String::new()])
    } else if let Some(encoded) = path.strip_prefix(COMPOUND_KEY_PATH) {
        serde_json::from_str(encoded).ok()
    } else {
        Some(vec![path.to_string()])
    }
}

fn normalize_key_path_storage(path: String) -> String {
    if path.starts_with(KEY_PATH_PREFIX) {
        path
    } else if path.is_empty() {
        encode_key_path(None)
    } else {
        encode_key_path(Some(&[path]))
    }
}

fn encode_utf16(value: &str) -> String {
    value
        .encode_utf16()
        .map(|unit| format!("{unit:04x}"))
        .collect()
}

fn extract_key_path(value: &Value, path: &str) -> Option<Value> {
    let parts = key_path_parts(path)?;
    if parts.len() > 1 {
        return parts
            .iter()
            .map(|part| extract_single_key_path(value, part))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array);
    }
    extract_single_key_path(value, &parts[0])
}

fn extract_single_key_path(value: &Value, path: &str) -> Option<Value> {
    if value.get("\u{1f}w3cos-idb-clone").and_then(Value::as_str) == Some("graph") {
        return extract_graph_key_path(value, path);
    }
    if path.is_empty() {
        return Some(value.clone());
    }
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

fn extract_graph_key_path(graph: &Value, path: &str) -> Option<Value> {
    let nodes = graph.get("nodes")?.as_array()?;
    let mut current = graph.get("root")?;
    if path.is_empty() {
        return resolve_graph_value(current, nodes).cloned();
    }
    for segment in path.split('.') {
        current = resolve_graph_value(current, nodes)?;
        let properties = if current.get("kind").and_then(Value::as_str) == Some("object") {
            current.get("value")?.as_object()?
        } else {
            current.as_object()?
        };
        current = properties.get(segment)?;
    }
    let resolved = resolve_graph_value(current, nodes)?;
    if resolved.get("kind").is_some() {
        None
    } else {
        Some(resolved.clone())
    }
}

fn resolve_graph_value<'a>(mut value: &'a Value, nodes: &'a [Value]) -> Option<&'a Value> {
    let mut remaining = nodes.len() + 1;
    while value.get("\u{1f}w3cos-idb-clone").and_then(Value::as_str) == Some("ref") {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        value = nodes.get(value.get("id")?.as_u64()? as usize)?;
    }
    Some(value)
}

fn inject_key_path(value: &mut Value, path: &str, new_value: Value) {
    let Some(parts) = key_path_parts(path) else {
        return;
    };
    if parts.len() != 1 || parts[0].is_empty() {
        return;
    }
    let segments: Vec<&str> = parts[0].split('.').collect();
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
            .and_then(|m| {
                Some(
                    m.entry(segment.to_string())
                        .or_insert(Value::Object(serde_json::Map::new())),
                )
            })
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("w3cos-idb-{}-{}", std::process::id(), label,));
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
        assert_eq!(key, json!(1));
        let key2 = store.put(json!({ "text": "world" })).unwrap();
        assert_eq!(key2, json!(2));

        let got = store.get(&json!(1)).unwrap().unwrap();
        assert_eq!(got["text"], json!("hello"));
        assert_eq!(store.count().unwrap(), 2);
        tx.commit().unwrap();
    }

    #[test]
    fn add_rejects_duplicates() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("dup"));
        let db = open("dup", 1, |db, _, _| {
            db.create_object_store("s", "id", false)
        })
        .unwrap();
        let tx = db.transaction(&["s"], TransactionMode::ReadWrite).unwrap();
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
        assert_eq!(err.name, "ReadOnlyError");
    }

    #[test]
    fn keys_keep_their_type_and_indexeddb_order() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("key-order"));
        let db = open("key-order", 1, |db, _, _| {
            db.create_object_store("s", "", false)
        })
        .unwrap();
        let tx = db.transaction(&["s"], TransactionMode::ReadWrite).unwrap();
        let store = tx.object_store("s").unwrap();
        for key in [json!(10), json!(-10), json!(2), json!("1"), json!("10")] {
            store
                .put_with_key(json!({ "key": key }), Some(key))
                .unwrap();
        }
        let keys = store.get_all_keys().unwrap();
        assert_eq!(
            keys,
            vec![json!(-10), json!(2), json!(10), json!("1"), json!("10")]
        );
        assert!(store.get(&json!(10)).unwrap().is_some());
        assert!(store.get(&json!("10")).unwrap().is_some());
        let range = KeyRange::bound(&json!(2), &json!(10), false, false).unwrap();
        assert_eq!(
            store.get_all_keys_range(Some(&range), None).unwrap(),
            vec![json!(2), json!(10)]
        );
        assert_eq!(
            store
                .put_with_key(json!({}), Some(json!(true)))
                .unwrap_err()
                .name,
            "DataError"
        );
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
        store
            .put(json!({ "email": "a@x.com", "name": "A" }))
            .unwrap();
        store
            .put(json!({ "email": "b@x.com", "name": "B" }))
            .unwrap();
        store
            .put(json!({ "email": "a@x.com", "name": "A2" }))
            .unwrap();

        let hits = store.index_get("by_email", &json!("a@x.com")).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h["name"] == json!("A2")));
        tx.commit().unwrap();
    }

    #[test]
    fn unique_and_multi_entry_indexes_are_enforced() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("index-options"));
        let db = open("index-options", 1, |db, _, _| {
            db.create_object_store("items", "id", false)?;
            db.create_index_with_options("items", "by_email", "email", true, false)?;
            db.create_index_with_options("items", "by_tag", "tags", false, true)
        })
        .unwrap();
        let tx = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        let store = tx.object_store("items").unwrap();
        store
            .add(json!({ "id": 1, "email": "a@x", "tags": ["red", "fast", "red"] }))
            .unwrap();
        store
            .add(json!({ "id": 2, "email": "b@x", "tags": ["red"] }))
            .unwrap();
        let red = KeyRange::only(&json!("red")).unwrap();
        let entries = store.index_scan("by_tag", Some(&red)).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, json!(1));
        assert_eq!(entries[1].1, json!(2));
        assert_eq!(
            store
                .add(json!({ "id": 3, "email": "a@x", "tags": [] }))
                .unwrap_err()
                .name,
            "ConstraintError"
        );
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
            tx.commit().unwrap();
        }
        // Re-open in a fresh process simulation by clearing the registry cache.
        set_base_dir(dir);
        let db = open("p", 1, |_, _, _| Ok(())).unwrap();
        let tx = db.transaction(&["s"], TransactionMode::ReadOnly).unwrap();
        let store = tx.object_store("s").unwrap();
        assert_eq!(store.count().unwrap(), 1);
        let row = store.get(&json!(1)).unwrap().unwrap();
        assert_eq!(row["v"], json!(42));

        let bytes = std::fs::read(registry().lock().unwrap().path_for("p")).unwrap();
        assert_eq!(&bytes[..16], b"SQLite format 3\0");
    }

    #[test]
    fn delete_database() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let dir = fresh_dir("del");
        set_base_dir(dir);
        let _ = open("trash", 1, |db, _, _| {
            db.create_object_store("s", "id", true)
        })
        .unwrap();
        delete("trash").unwrap();
        let dbs = databases();
        assert!(!dbs.iter().any(|d| d == "trash"));
    }

    #[test]
    fn abort_discards_every_write_in_the_transaction() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("abort"));
        let db = open("abort", 1, |db, _, _| {
            db.create_object_store("items", "id", false)
        })
        .unwrap();
        let transaction = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        let store = transaction.object_store("items").unwrap();
        store.put(json!({ "id": "one" })).unwrap();
        store.put(json!({ "id": "two" })).unwrap();
        transaction.abort().unwrap();

        let read = db
            .transaction(&["items"], TransactionMode::ReadOnly)
            .unwrap();
        assert_eq!(read.object_store("items").unwrap().count().unwrap(), 0);
        read.commit().unwrap();
    }

    #[test]
    fn commit_publishes_all_writes_at_once() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("atomic-commit"));
        let db = open("atomic", 1, |db, _, _| {
            db.create_object_store("items", "id", false)
        })
        .unwrap();
        let write = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        let store = write.object_store("items").unwrap();
        store.put(json!({ "id": "one" })).unwrap();

        let before = db
            .transaction(&["items"], TransactionMode::ReadOnly)
            .unwrap();
        assert_eq!(before.object_store("items").unwrap().count().unwrap(), 0);
        before.commit().unwrap();

        store.put(json!({ "id": "two" })).unwrap();
        write.commit().unwrap();

        let after = db
            .transaction(&["items"], TransactionMode::ReadOnly)
            .unwrap();
        assert_eq!(after.object_store("items").unwrap().count().unwrap(), 2);
        after.commit().unwrap();
    }

    #[test]
    fn failed_versionchange_restores_the_previous_schema_and_version() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let dir = fresh_dir("versionchange-rollback");
        set_base_dir(dir.clone());
        let db = open("schema", 1, |db, _, _| {
            db.create_object_store("stable", "id", false)
        })
        .unwrap();
        assert_eq!(db.version(), 1);

        let error = open("schema", 2, |db, _, _| {
            db.create_object_store("must_not_leak", "id", false)?;
            Err(IndexedDbError::data("upgrade failed"))
        })
        .err()
        .expect("upgrade must fail");
        assert_eq!(error.name, "DataError");

        assert_eq!(db.version(), 1);
        assert_eq!(db.object_store_names(), vec!["stable"]);
        set_base_dir(dir);
        let reopened = open("schema", 1, |_, _, _| Ok(())).unwrap();
        assert_eq!(reopened.version(), 1);
        assert_eq!(reopened.object_store_names(), vec!["stable"]);
    }

    #[test]
    fn sqlite_uses_normalized_tables_instead_of_the_legacy_snapshot() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let dir = fresh_dir("normalized");
        set_base_dir(dir);
        let db = open("normalized", 1, |db, _, _| {
            db.create_object_store("items", "id", false)?;
            db.create_index("items", "by_kind", "kind")
        })
        .unwrap();
        let transaction = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        transaction
            .object_store("items")
            .unwrap()
            .put(json!({"id": 1, "kind": "offline"}))
            .unwrap();
        transaction.commit().unwrap();
        let path = registry().lock().unwrap().path_for("normalized");
        assert_eq!(
            indexed_db_sqlite::table_row_count(&path, "w3cos_idb_state").unwrap(),
            0
        );
        assert_eq!(
            indexed_db_sqlite::table_row_count(&path, "w3cos_idb_object_store").unwrap(),
            1
        );
        assert_eq!(
            indexed_db_sqlite::table_row_count(&path, "w3cos_idb_record").unwrap(),
            1
        );
        assert_eq!(
            indexed_db_sqlite::table_row_count(&path, "w3cos_idb_index_entry").unwrap(),
            1
        );
    }

    #[test]
    fn database_names_cannot_escape_the_storage_scope_and_quota_fails_closed() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let dir = fresh_dir("scope-quota");
        set_base_dir(dir.clone());
        let db = open("../../outside", 1, |db, _, _| {
            db.create_object_store("items", "id", false)
        })
        .unwrap();
        assert_eq!(db.name(), "../../outside");
        let path = registry().lock().unwrap().path_for("../../outside");
        assert_eq!(path.parent(), Some(dir.as_path()));
        assert_eq!(databases(), vec!["../../outside"]);

        set_base_dir(fresh_dir("quota"));
        set_quota_bytes(1);
        let error = open("too-large", 1, |db, _, _| {
            db.create_object_store("items", "id", false)
        })
        .err()
        .expect("quota must reject the versionchange commit");
        assert_eq!(error.name, "QuotaExceededError");
        set_quota_bytes(256 * 1024 * 1024);
    }

    #[test]
    fn compound_key_paths_resolve_store_and_index_keys() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        set_base_dir(fresh_dir("compound-key-path"));
        let primary = encode_key_path(Some(&["org".into(), "id".into()]));
        let lookup = encode_key_path(Some(&["country".into(), "code".into()]));
        let db = open("compound", 1, |db, _, _| {
            db.create_object_store("items", primary, false)?;
            db.create_index("items", "by_lookup", lookup)
        })
        .unwrap();
        let transaction = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        let store = transaction.object_store("items").unwrap();
        let key = store
            .add(json!({"org": "acme", "id": 7, "country": "CN", "code": "SHA"}))
            .unwrap();
        assert_eq!(key, json!(["acme", 7]));
        assert!(store.get(&json!(["acme", 7])).unwrap().is_some());
        assert_eq!(
            store
                .index_get("by_lookup", &json!(["CN", "SHA"]))
                .unwrap()
                .len(),
            1
        );
        transaction.commit().unwrap();

        let invalid = encode_key_path(Some(&["a".into(), "b".into()]));
        let error = db
            .create_index_with_options("items", "invalid", invalid, false, true)
            .unwrap_err();
        assert_eq!(error.name, "InvalidAccessError");
    }

    #[test]
    fn process_termination_child() {
        let Ok(directory) = std::env::var("W3COS_IDB_TERMINATION_DIR") else {
            return;
        };
        let mode = std::env::var("W3COS_IDB_TERMINATION_MODE").unwrap();
        set_base_dir(PathBuf::from(directory));
        let db = open(&mode, 1, |db, _, _| {
            db.create_object_store("items", "id", false)
        })
        .unwrap();
        let transaction = db
            .transaction(&["items"], TransactionMode::ReadWrite)
            .unwrap();
        transaction
            .object_store("items")
            .unwrap()
            .put(json!({"id": 1, "state": mode}))
            .unwrap();
        if mode == "committed" {
            transaction.commit().unwrap();
        }
        std::process::abort();
    }

    #[test]
    fn abrupt_process_termination_preserves_committed_and_discards_uncommitted_writes() {
        let _g = IDB_TEST_LOCK.lock().unwrap();
        let directory = fresh_dir("process-termination");
        for (mode, expected_count) in [("committed", 1), ("uncommitted", 0)] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("indexed_db::tests::process_termination_child")
                .arg("--nocapture")
                .env("W3COS_IDB_TERMINATION_DIR", &directory)
                .env("W3COS_IDB_TERMINATION_MODE", mode)
                .status()
                .unwrap();
            assert!(!status.success(), "child must terminate abruptly");

            set_base_dir(directory.clone());
            let reopened = open(mode, 1, |_, _, _| Ok(())).unwrap();
            let read = reopened
                .transaction(&["items"], TransactionMode::ReadOnly)
                .unwrap();
            assert_eq!(
                read.object_store("items").unwrap().count().unwrap(),
                expected_count
            );
            read.commit().unwrap();
        }
    }
}
