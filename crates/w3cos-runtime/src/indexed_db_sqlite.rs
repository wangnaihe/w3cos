//! Minimal SQLite persistence used by the IndexedDB user-agent implementation.
//!
//! This module intentionally exposes no JavaScript API. IndexedDB remains the
//! only application-facing contract; SQLite is an internal durability layer.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr;

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

// Keep the bundled Android SQLite dependency linked even though this module
// deliberately owns the small FFI surface used by IndexedDB.
#[cfg(target_os = "android")]
use libsqlite3_sys as _;

use crate::indexed_db::{
    DatabaseState, IndexState, IndexedDbError, ObjectStoreState, Result, StoredRecord,
    canonicalize_key, index_keys,
};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

type SqliteDestructor = Option<unsafe extern "C" fn(*mut c_void)>;

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        database: *mut *mut Sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(database: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(database: *mut Sqlite3) -> *const c_char;
    fn sqlite3_exec(
        database: *mut Sqlite3,
        sql: *const c_char,
        callback: *mut c_void,
        callback_arg: *mut c_void,
        error_message: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        database: *mut Sqlite3,
        sql: *const c_char,
        sql_bytes: c_int,
        statement: *mut *mut Sqlite3Stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_blob(
        statement: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_void,
        bytes: c_int,
        destructor: SqliteDestructor,
    ) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_char,
        bytes: c_int,
        destructor: SqliteDestructor,
    ) -> c_int;
    fn sqlite3_bind_int64(statement: *mut Sqlite3Stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_step(statement: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_column_blob(statement: *mut Sqlite3Stmt, column: c_int) -> *const c_void;
    fn sqlite3_column_bytes(statement: *mut Sqlite3Stmt, column: c_int) -> c_int;
    fn sqlite3_column_text(statement: *mut Sqlite3Stmt, column: c_int) -> *const u8;
    fn sqlite3_column_int64(statement: *mut Sqlite3Stmt, column: c_int) -> i64;
    fn sqlite3_finalize(statement: *mut Sqlite3Stmt) -> c_int;
}

struct Connection(*mut Sqlite3);

impl Connection {
    fn open(path: &Path) -> Result<Self> {
        let path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|error| storage_error(format!("invalid database path: {error}")))?;
        let mut database = ptr::null_mut();
        // SAFETY: `path` is NUL-terminated and `database` points to writable storage.
        let status = unsafe {
            sqlite3_open_v2(
                path.as_ptr(),
                &mut database,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX,
                ptr::null(),
            )
        };
        if status != SQLITE_OK || database.is_null() {
            let message = if database.is_null() {
                format!("sqlite open failed with status {status}")
            } else {
                sqlite_error(database)
            };
            if !database.is_null() {
                // SAFETY: SQLite returned an initialized handle.
                unsafe { sqlite3_close(database) };
            }
            return Err(storage_error(message));
        }
        let connection = Self(database);
        connection.exec("PRAGMA journal_mode=WAL")?;
        connection.exec("PRAGMA synchronous=FULL")?;
        connection.exec("PRAGMA foreign_keys=ON")?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS w3cos_idb_state (\
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),\
             payload BLOB NOT NULL)",
        )?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS w3cos_idb_meta (\
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),\
             name TEXT NOT NULL, version INTEGER NOT NULL)",
        )?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS w3cos_idb_object_store (\
             name TEXT PRIMARY KEY, key_path TEXT NOT NULL,\
             auto_increment INTEGER NOT NULL, next_key INTEGER NOT NULL)",
        )?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS w3cos_idb_index (\
             store_name TEXT NOT NULL, name TEXT NOT NULL, key_path TEXT NOT NULL,\
             unique_flag INTEGER NOT NULL, multi_entry INTEGER NOT NULL,\
             PRIMARY KEY(store_name, name),\
             FOREIGN KEY(store_name) REFERENCES w3cos_idb_object_store(name) ON DELETE CASCADE)",
        )?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS w3cos_idb_record (\
             store_name TEXT NOT NULL, canonical_key TEXT NOT NULL,\
             key_json BLOB NOT NULL, value_json BLOB NOT NULL,\
             PRIMARY KEY(store_name, canonical_key),\
             FOREIGN KEY(store_name) REFERENCES w3cos_idb_object_store(name) ON DELETE CASCADE)",
        )?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS w3cos_idb_index_entry (\
             store_name TEXT NOT NULL, index_name TEXT NOT NULL,\
             canonical_index_key TEXT NOT NULL, canonical_primary_key TEXT NOT NULL,\
             index_key_json BLOB NOT NULL,\
             PRIMARY KEY(store_name, index_name, canonical_index_key, canonical_primary_key),\
             FOREIGN KEY(store_name, index_name) REFERENCES w3cos_idb_index(store_name, name) ON DELETE CASCADE)",
        )?;
        Ok(connection)
    }

    fn exec(&self, sql: &str) -> Result<()> {
        let sql = CString::new(sql).map_err(|error| storage_error(error.to_string()))?;
        // SAFETY: the connection and SQL string remain valid for the duration of the call.
        let status = unsafe {
            sqlite3_exec(
                self.0,
                sql.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status == SQLITE_OK {
            Ok(())
        } else {
            Err(storage_error(sqlite_error(self.0)))
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the connection handle.
        unsafe { sqlite3_close(self.0) };
    }
}

struct Statement(*mut Sqlite3Stmt);

impl Statement {
    fn prepare(connection: &Connection, sql: &str) -> Result<Self> {
        let sql = CString::new(sql).map_err(|error| storage_error(error.to_string()))?;
        let mut statement = ptr::null_mut();
        // SAFETY: SQLite reads the NUL-terminated SQL and initializes `statement`.
        let status = unsafe {
            sqlite3_prepare_v2(
                connection.0,
                sql.as_ptr(),
                -1,
                &mut statement,
                ptr::null_mut(),
            )
        };
        if status != SQLITE_OK {
            return Err(storage_error(sqlite_error(connection.0)));
        }
        Ok(Self(statement))
    }

    fn bind_text(&self, connection: &Connection, index: c_int, value: &str) -> Result<()> {
        let value = CString::new(value).map_err(|error| storage_error(error.to_string()))?;
        let status = unsafe {
            sqlite3_bind_text(
                self.0,
                index,
                value.as_ptr(),
                value
                    .as_bytes()
                    .len()
                    .try_into()
                    .map_err(|_| storage_error("text too large"))?,
                sqlite_transient(),
            )
        };
        check_status(connection, status)
    }

    fn bind_blob(&self, connection: &Connection, index: c_int, value: &[u8]) -> Result<()> {
        let status = unsafe {
            sqlite3_bind_blob(
                self.0,
                index,
                value.as_ptr().cast(),
                value
                    .len()
                    .try_into()
                    .map_err(|_| storage_error("blob too large"))?,
                sqlite_transient(),
            )
        };
        check_status(connection, status)
    }

    fn bind_i64(&self, connection: &Connection, index: c_int, value: i64) -> Result<()> {
        let status = unsafe { sqlite3_bind_int64(self.0, index, value) };
        check_status(connection, status)
    }

    fn execute(&self, connection: &Connection) -> Result<()> {
        if unsafe { sqlite3_step(self.0) } == SQLITE_DONE {
            Ok(())
        } else {
            Err(storage_error(sqlite_error(connection.0)))
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the prepared statement.
        unsafe { sqlite3_finalize(self.0) };
    }
}

fn legacy_read(path: &Path) -> Result<Option<Vec<u8>>> {
    let connection = Connection::open(path)?;
    let statement = Statement::prepare(
        &connection,
        "SELECT payload FROM w3cos_idb_state WHERE singleton = 1",
    )?;
    // SAFETY: the prepared statement remains alive while the row is inspected.
    match unsafe { sqlite3_step(statement.0) } {
        SQLITE_DONE => Ok(None),
        SQLITE_ROW => {
            // SAFETY: column pointers are valid until the statement advances/finalizes.
            let bytes = unsafe { sqlite3_column_bytes(statement.0, 0) };
            let blob = unsafe { sqlite3_column_blob(statement.0, 0) };
            if bytes < 0 || (blob.is_null() && bytes != 0) {
                return Err(storage_error("SQLite returned an invalid state payload"));
            }
            // SAFETY: SQLite reports `bytes` readable bytes at `blob`.
            Ok(Some(unsafe {
                std::slice::from_raw_parts(blob.cast::<u8>(), bytes as usize).to_vec()
            }))
        }
        _ => Err(storage_error(sqlite_error(connection.0))),
    }
}

pub(crate) fn read_state(path: &Path) -> Result<Option<DatabaseState>> {
    let connection = Connection::open(path)?;
    let meta = Statement::prepare(
        &connection,
        "SELECT name, version FROM w3cos_idb_meta WHERE singleton = 1",
    )?;
    let status = unsafe { sqlite3_step(meta.0) };
    if status == SQLITE_DONE {
        drop(meta);
        drop(connection);
        let Some(payload) = legacy_read(path)? else {
            return Ok(None);
        };
        let state = serde_json::from_slice::<DatabaseState>(&payload)
            .map_err(|error| storage_error(format!("corrupt legacy database state: {error}")))?;
        write_state(path, &state)?;
        return Ok(Some(state));
    }
    if status != SQLITE_ROW {
        return Err(storage_error(sqlite_error(connection.0)));
    }
    let name = column_text(&meta, 0)?;
    let version = unsafe { sqlite3_column_int64(meta.0, 1) };
    if !(0..=u32::MAX as i64).contains(&version) {
        return Err(storage_error("invalid IndexedDB version"));
    }
    drop(meta);

    let mut stores = HashMap::new();
    let statement = Statement::prepare(
        &connection,
        "SELECT name, key_path, auto_increment, next_key FROM w3cos_idb_object_store ORDER BY name",
    )?;
    loop {
        match unsafe { sqlite3_step(statement.0) } {
            SQLITE_DONE => break,
            SQLITE_ROW => {
                let store_name = column_text(&statement, 0)?;
                stores.insert(
                    store_name.clone(),
                    ObjectStoreState {
                        name: store_name,
                        key_path: column_text(&statement, 1)?,
                        auto_increment: unsafe { sqlite3_column_int64(statement.0, 2) } != 0,
                        next_key: unsafe { sqlite3_column_int64(statement.0, 3) },
                        indexes: HashMap::new(),
                        records: BTreeMap::new(),
                    },
                );
            }
            _ => return Err(storage_error(sqlite_error(connection.0))),
        }
    }

    let statement = Statement::prepare(
        &connection,
        "SELECT store_name, name, key_path, unique_flag, multi_entry FROM w3cos_idb_index ORDER BY store_name, name",
    )?;
    loop {
        match unsafe { sqlite3_step(statement.0) } {
            SQLITE_DONE => break,
            SQLITE_ROW => {
                let store_name = column_text(&statement, 0)?;
                let index_name = column_text(&statement, 1)?;
                let store = stores
                    .get_mut(&store_name)
                    .ok_or_else(|| storage_error("index references a missing object store"))?;
                store.indexes.insert(
                    index_name,
                    IndexState {
                        key_path: column_text(&statement, 2)?,
                        unique: unsafe { sqlite3_column_int64(statement.0, 3) } != 0,
                        multi_entry: unsafe { sqlite3_column_int64(statement.0, 4) } != 0,
                    },
                );
            }
            _ => return Err(storage_error(sqlite_error(connection.0))),
        }
    }

    let statement = Statement::prepare(
        &connection,
        "SELECT store_name, canonical_key, key_json, value_json FROM w3cos_idb_record ORDER BY store_name, canonical_key",
    )?;
    loop {
        match unsafe { sqlite3_step(statement.0) } {
            SQLITE_DONE => break,
            SQLITE_ROW => {
                let store_name = column_text(&statement, 0)?;
                let canonical_key = column_text(&statement, 1)?;
                let key = serde_json::from_slice::<Value>(&column_blob(&statement, 2)?)
                    .map_err(|error| storage_error(format!("corrupt record key: {error}")))?;
                let value = serde_json::from_slice::<Value>(&column_blob(&statement, 3)?)
                    .map_err(|error| storage_error(format!("corrupt record value: {error}")))?;
                stores
                    .get_mut(&store_name)
                    .ok_or_else(|| storage_error("record references a missing object store"))?
                    .records
                    .insert(canonical_key, StoredRecord { key, value });
            }
            _ => return Err(storage_error(sqlite_error(connection.0))),
        }
    }
    Ok(Some(DatabaseState {
        name,
        version: version as u32,
        stores,
    }))
}

pub(crate) fn write_state(path: &Path, state: &DatabaseState) -> Result<()> {
    let connection = Connection::open(path)?;
    connection.exec("BEGIN IMMEDIATE")?;
    let result = (|| {
        for table in [
            "w3cos_idb_index_entry",
            "w3cos_idb_record",
            "w3cos_idb_index",
            "w3cos_idb_object_store",
            "w3cos_idb_meta",
        ] {
            connection.exec(&format!("DELETE FROM {table}"))?;
        }

        let meta = Statement::prepare(
            &connection,
            "INSERT INTO w3cos_idb_meta(singleton, name, version) VALUES(1, ?1, ?2)",
        )?;
        meta.bind_text(&connection, 1, &state.name)?;
        meta.bind_i64(&connection, 2, state.version as i64)?;
        meta.execute(&connection)?;

        for store in state.stores.values() {
            let statement = Statement::prepare(
                &connection,
                "INSERT INTO w3cos_idb_object_store(name, key_path, auto_increment, next_key) VALUES(?1, ?2, ?3, ?4)",
            )?;
            statement.bind_text(&connection, 1, &store.name)?;
            statement.bind_text(&connection, 2, &store.key_path)?;
            statement.bind_i64(&connection, 3, i64::from(store.auto_increment))?;
            statement.bind_i64(&connection, 4, store.next_key)?;
            statement.execute(&connection)?;

            for (index_name, index) in &store.indexes {
                let statement = Statement::prepare(
                    &connection,
                    "INSERT INTO w3cos_idb_index(store_name, name, key_path, unique_flag, multi_entry) VALUES(?1, ?2, ?3, ?4, ?5)",
                )?;
                statement.bind_text(&connection, 1, &store.name)?;
                statement.bind_text(&connection, 2, index_name)?;
                statement.bind_text(&connection, 3, &index.key_path)?;
                statement.bind_i64(&connection, 4, i64::from(index.unique))?;
                statement.bind_i64(&connection, 5, i64::from(index.multi_entry))?;
                statement.execute(&connection)?;
            }

            for (canonical_primary_key, record) in &store.records {
                let key_json = serde_json::to_vec(&record.key)
                    .map_err(|error| storage_error(error.to_string()))?;
                let value_json = serde_json::to_vec(&record.value)
                    .map_err(|error| storage_error(error.to_string()))?;
                let statement = Statement::prepare(
                    &connection,
                    "INSERT INTO w3cos_idb_record(store_name, canonical_key, key_json, value_json) VALUES(?1, ?2, ?3, ?4)",
                )?;
                statement.bind_text(&connection, 1, &store.name)?;
                statement.bind_text(&connection, 2, canonical_primary_key)?;
                statement.bind_blob(&connection, 3, &key_json)?;
                statement.bind_blob(&connection, 4, &value_json)?;
                statement.execute(&connection)?;

                for (index_name, index) in &store.indexes {
                    for index_key in index_keys(index, &record.value) {
                        let canonical_index_key = canonicalize_key(&index_key)?;
                        let index_key_json = serde_json::to_vec(&index_key)
                            .map_err(|error| storage_error(error.to_string()))?;
                        let statement = Statement::prepare(
                            &connection,
                            "INSERT INTO w3cos_idb_index_entry(store_name, index_name, canonical_index_key, canonical_primary_key, index_key_json) VALUES(?1, ?2, ?3, ?4, ?5)",
                        )?;
                        statement.bind_text(&connection, 1, &store.name)?;
                        statement.bind_text(&connection, 2, index_name)?;
                        statement.bind_text(&connection, 3, &canonical_index_key)?;
                        statement.bind_text(&connection, 4, canonical_primary_key)?;
                        statement.bind_blob(&connection, 5, &index_key_json)?;
                        statement.execute(&connection)?;
                    }
                }
            }
        }
        connection.exec("DELETE FROM w3cos_idb_state")?;
        Ok(())
    })();
    match result {
        Ok(()) => connection.exec("COMMIT"),
        Err(error) => {
            let _ = connection.exec("ROLLBACK");
            Err(error)
        }
    }
}

fn sqlite_transient() -> SqliteDestructor {
    unsafe { std::mem::transmute(-1_isize) }
}

fn check_status(connection: &Connection, status: c_int) -> Result<()> {
    if status == SQLITE_OK {
        Ok(())
    } else {
        Err(storage_error(sqlite_error(connection.0)))
    }
}

fn column_text(statement: &Statement, column: c_int) -> Result<String> {
    let bytes = unsafe { sqlite3_column_bytes(statement.0, column) };
    let text = unsafe { sqlite3_column_text(statement.0, column) };
    if bytes < 0 || (text.is_null() && bytes != 0) {
        return Err(storage_error("SQLite returned invalid text"));
    }
    Ok(
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(text, bytes as usize) })
            .into_owned(),
    )
}

fn column_blob(statement: &Statement, column: c_int) -> Result<Vec<u8>> {
    let bytes = unsafe { sqlite3_column_bytes(statement.0, column) };
    let blob = unsafe { sqlite3_column_blob(statement.0, column) };
    if bytes < 0 || (blob.is_null() && bytes != 0) {
        return Err(storage_error("SQLite returned invalid blob"));
    }
    Ok(unsafe { std::slice::from_raw_parts(blob.cast::<u8>(), bytes as usize) }.to_vec())
}

fn sqlite_error(database: *mut Sqlite3) -> String {
    // SAFETY: SQLite returns a connection-owned, NUL-terminated error string.
    unsafe { CStr::from_ptr(sqlite3_errmsg(database)) }
        .to_string_lossy()
        .into_owned()
}

fn storage_error(message: impl Into<String>) -> IndexedDbError {
    IndexedDbError {
        name: "UnknownError".into(),
        message: message.into(),
    }
}

#[cfg(test)]
pub(crate) fn table_row_count(path: &Path, table: &str) -> Result<i64> {
    let allowed = [
        "w3cos_idb_state",
        "w3cos_idb_meta",
        "w3cos_idb_object_store",
        "w3cos_idb_index",
        "w3cos_idb_record",
        "w3cos_idb_index_entry",
    ];
    if !allowed.contains(&table) {
        return Err(storage_error("unknown test table"));
    }
    let connection = Connection::open(path)?;
    let statement = Statement::prepare(&connection, &format!("SELECT COUNT(*) FROM {table}"))?;
    if unsafe { sqlite3_step(statement.0) } != SQLITE_ROW {
        return Err(storage_error(sqlite_error(connection.0)));
    }
    Ok(unsafe { sqlite3_column_int64(statement.0, 0) })
}
