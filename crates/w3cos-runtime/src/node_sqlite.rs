//! SQLite for `@vscode/sqlite3` / `@w3cos/sqlite3` (M3 item 20).
//!
//! The node-sqlite3 API (Database, Statement, callbacks, events) is
//! CommonJS glue in `w3cos-node/js/sqlite3.js`. This module is the host half
//! on `__w3cos_node_host`: synchronous calls on connections owned by the
//! calling thread. Each returns a plain object. Failures come back as
//! `{ error: { message, code, errno } }` in node-sqlite3's shape
//! (`SQLITE_ERROR: no such table: x`) instead of throwing, because the glue
//! hands them to callbacks or `'error'` events.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

use w3cos_core::Value;

// Keep the bundled Android SQLite dependency linked (see indexed_db_sqlite).
#[cfg(target_os = "android")]
use libsqlite3_sys as _;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_INTEGER: c_int = 1;
const SQLITE_FLOAT: c_int = 2;
const SQLITE_TEXT: c_int = 3;
const SQLITE_BLOB: c_int = 4;
const SQLITE_MISUSE: c_int = 21;

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

type Destructor = Option<unsafe extern "C" fn(*mut c_void)>;

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
    fn sqlite3_errcode(database: *mut Sqlite3) -> c_int;
    fn sqlite3_errstr(code: c_int) -> *const c_char;
    fn sqlite3_exec(
        database: *mut Sqlite3,
        sql: *const c_char,
        callback: *mut c_void,
        argument: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(pointer: *mut c_void);
    fn sqlite3_prepare_v2(
        database: *mut Sqlite3,
        sql: *const c_char,
        bytes: c_int,
        statement: *mut *mut Sqlite3Stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_parameter_count(statement: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_bind_parameter_index(statement: *mut Sqlite3Stmt, name: *const c_char) -> c_int;
    fn sqlite3_bind_null(statement: *mut Sqlite3Stmt, index: c_int) -> c_int;
    fn sqlite3_bind_int64(statement: *mut Sqlite3Stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_bind_double(statement: *mut Sqlite3Stmt, index: c_int, value: f64) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_char,
        bytes: c_int,
        destructor: Destructor,
    ) -> c_int;
    fn sqlite3_bind_blob(
        statement: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_void,
        bytes: c_int,
        destructor: Destructor,
    ) -> c_int;
    fn sqlite3_step(statement: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_column_count(statement: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_column_name(statement: *mut Sqlite3Stmt, column: c_int) -> *const c_char;
    fn sqlite3_column_type(statement: *mut Sqlite3Stmt, column: c_int) -> c_int;
    fn sqlite3_column_int64(statement: *mut Sqlite3Stmt, column: c_int) -> i64;
    fn sqlite3_column_double(statement: *mut Sqlite3Stmt, column: c_int) -> f64;
    fn sqlite3_column_text(statement: *mut Sqlite3Stmt, column: c_int) -> *const u8;
    fn sqlite3_column_blob(statement: *mut Sqlite3Stmt, column: c_int) -> *const c_void;
    fn sqlite3_column_bytes(statement: *mut Sqlite3Stmt, column: c_int) -> c_int;
    fn sqlite3_finalize(statement: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_changes(database: *mut Sqlite3) -> c_int;
    fn sqlite3_last_insert_rowid(database: *mut Sqlite3) -> i64;
    fn sqlite3_busy_timeout(database: *mut Sqlite3, milliseconds: c_int) -> c_int;
}

/// SQLITE_TRANSIENT: SQLite copies bound text and blobs.
fn transient() -> Destructor {
    // SAFETY: SQLite defines SQLITE_TRANSIENT as the sentinel pointer -1; it
    // is never called as a function.
    Some(unsafe { std::mem::transmute::<isize, unsafe extern "C" fn(*mut c_void)>(-1) })
}

struct Connection(*mut Sqlite3);

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { sqlite3_close(self.0) };
    }
}

thread_local! {
    static CONNECTIONS: RefCell<HashMap<i64, Connection>> = RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<i64> = const { std::cell::Cell::new(1) };
}

/// Install the `sqlite*` host functions (registered from the runtime).
pub(crate) fn install(host: &Value) {
    use w3cos_core::node_host::define;
    define(host, "sqliteOpen", |args| {
        let path = arg(&args, 0).to_js_string();
        let flags = number(&arg(&args, 1)).unwrap_or(0x0001_0006 as f64) as c_int;
        open(&path, flags)
    });
    define(host, "sqliteExec", |args| {
        let id = id_arg(&args);
        let sql = arg(&args, 1).to_js_string();
        with_connection(id, |db| exec(db, &sql))
    });
    define(host, "sqliteQuery", |args| {
        let id = id_arg(&args);
        let sql = arg(&args, 1).to_js_string();
        let (positional, named) = (arg(&args, 2), arg(&args, 3));
        let want_rows = arg(&args, 4).to_bool();
        with_connection(id, |db| query(db, &sql, &positional, &named, want_rows))
    });
    define(host, "sqliteBusyTimeout", |args| {
        let id = id_arg(&args);
        let ms = number(&arg(&args, 1)).unwrap_or(0.0) as c_int;
        with_connection(id, |db| {
            unsafe { sqlite3_busy_timeout(db, ms) };
            ok_object()
        })
    });
    define(host, "sqliteClose", |args| {
        let id = id_arg(&args);
        match CONNECTIONS.with(|c| c.borrow_mut().remove(&id)) {
            Some(connection) => {
                let db = connection.0;
                let status = unsafe { sqlite3_close(db) };
                if status == SQLITE_OK {
                    std::mem::forget(connection);
                    ok_object()
                } else {
                    let error = error_value(status, &last_message(db));
                    // Still open (unfinalized statements): keep it.
                    CONNECTIONS.with(|c| c.borrow_mut().insert(id, connection));
                    error
                }
            }
            None => misuse("Database is closed"),
        }
    });
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn number(value: &Value) -> Option<f64> {
    value.as_number()
}

fn id_arg(args: &[Value]) -> i64 {
    number(&arg(args, 0)).unwrap_or(0.0) as i64
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    let value = Value::object(HashMap::new());
    for (key, item) in entries {
        value.set_property(key, item);
    }
    value
}

fn ok_object() -> Value {
    object(vec![])
}

/// node-sqlite3's error code names (primary result codes).
fn code_name(status: c_int) -> &'static str {
    const NAMES: [&str; 29] = [
        "SQLITE_OK",
        "SQLITE_ERROR",
        "SQLITE_INTERNAL",
        "SQLITE_PERM",
        "SQLITE_ABORT",
        "SQLITE_BUSY",
        "SQLITE_LOCKED",
        "SQLITE_NOMEM",
        "SQLITE_READONLY",
        "SQLITE_INTERRUPT",
        "SQLITE_IOERR",
        "SQLITE_CORRUPT",
        "SQLITE_NOTFOUND",
        "SQLITE_FULL",
        "SQLITE_CANTOPEN",
        "SQLITE_PROTOCOL",
        "SQLITE_EMPTY",
        "SQLITE_SCHEMA",
        "SQLITE_TOOBIG",
        "SQLITE_CONSTRAINT",
        "SQLITE_MISMATCH",
        "SQLITE_MISUSE",
        "SQLITE_NOLFS",
        "SQLITE_AUTH",
        "SQLITE_FORMAT",
        "SQLITE_RANGE",
        "SQLITE_NOTADB",
        "SQLITE_NOTICE",
        "SQLITE_WARNING",
    ];
    NAMES
        .get((status & 0xff) as usize)
        .copied()
        .unwrap_or("SQLITE_ERROR")
}

fn error_value(status: c_int, message: &str) -> Value {
    let primary = status & 0xff;
    let code = code_name(primary);
    object(vec![(
        "error",
        object(vec![
            ("message", Value::from(format!("{code}: {message}"))),
            ("code", Value::from(code)),
            ("errno", Value::Number(f64::from(primary))),
        ]),
    )])
}

fn misuse(message: &str) -> Value {
    error_value(SQLITE_MISUSE, message)
}

fn last_message(db: *mut Sqlite3) -> String {
    let text = unsafe { sqlite3_errmsg(db) };
    if text.is_null() {
        let text = unsafe { sqlite3_errstr(sqlite3_errcode(db)) };
        if text.is_null() {
            return String::new();
        }
        return unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned();
    }
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

fn db_error(db: *mut Sqlite3) -> Value {
    error_value(unsafe { sqlite3_errcode(db) }, &last_message(db))
}

fn open(path: &str, flags: c_int) -> Value {
    let Ok(c_path) = CString::new(path) else {
        return misuse("path contains a NUL byte");
    };
    let mut db = ptr::null_mut();
    let status = unsafe { sqlite3_open_v2(c_path.as_ptr(), &mut db, flags, ptr::null()) };
    if status != SQLITE_OK {
        let error = if db.is_null() {
            let text = unsafe { sqlite3_errstr(status) };
            let message = if text.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(text) }
                    .to_string_lossy()
                    .into_owned()
            };
            error_value(status, &message)
        } else {
            let error = error_value(status, &last_message(db));
            unsafe { sqlite3_close(db) };
            error
        };
        return error;
    }
    let id = NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    CONNECTIONS.with(|c| c.borrow_mut().insert(id, Connection(db)));
    object(vec![("id", Value::Number(id as f64))])
}

fn with_connection(id: i64, f: impl FnOnce(*mut Sqlite3) -> Value) -> Value {
    let db = CONNECTIONS.with(|c| c.borrow().get(&id).map(|connection| connection.0));
    match db {
        Some(db) => f(db),
        None => misuse("Database is closed"),
    }
}

fn exec(db: *mut Sqlite3, sql: &str) -> Value {
    let Ok(c_sql) = CString::new(sql) else {
        return misuse("SQL contains a NUL byte");
    };
    let mut message = ptr::null_mut();
    let status = unsafe {
        sqlite3_exec(
            db,
            c_sql.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut message,
        )
    };
    if status == SQLITE_OK {
        return ok_object();
    }
    let text = if message.is_null() {
        last_message(db)
    } else {
        let text = unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned();
        unsafe { sqlite3_free(message.cast()) };
        text
    };
    error_value(status, &text)
}

struct Statement(*mut Sqlite3Stmt);

impl Drop for Statement {
    fn drop(&mut self) {
        unsafe { sqlite3_finalize(self.0) };
    }
}

/// Bind one value. The glue has already normalized it the way node-sqlite3
/// does (booleans and Dates to numbers, Buffers and typed arrays kept as
/// bytes, other objects to strings).
fn bind(statement: *mut Sqlite3Stmt, index: c_int, value: &Value) -> c_int {
    unsafe {
        if value.is_null() || value.is_undefined() {
            return sqlite3_bind_null(statement, index);
        }
        if let Some(n) = value.as_number() {
            return if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                sqlite3_bind_int64(statement, index, n as i64)
            } else {
                sqlite3_bind_double(statement, index, n)
            };
        }
        if !value.is_string() {
            if let Some(bytes) = w3cos_core::binary::bytes_of(value) {
                return sqlite3_bind_blob(
                    statement,
                    index,
                    bytes.as_ptr().cast(),
                    bytes.len() as c_int,
                    transient(),
                );
            }
        }
        let text = value.to_js_string();
        sqlite3_bind_text(
            statement,
            index,
            text.as_ptr().cast(),
            text.len() as c_int,
            transient(),
        )
    }
}

fn length_of(value: &Value) -> usize {
    value
        .get_property("length")
        .as_number()
        .unwrap_or(0.0)
        .max(0.0) as usize
}

/// `positional`: an array bound to `?1..?n`. `named`: an array of
/// `[name, value]` pairs, names with their prefix (`$k`, `:k`, `@k`) or
/// 1-based positions.
fn bind_params(
    db: *mut Sqlite3,
    statement: *mut Sqlite3Stmt,
    positional: &Value,
    named: &Value,
) -> Option<Value> {
    let count = unsafe { sqlite3_bind_parameter_count(statement) };
    let check = |status: c_int| (status != SQLITE_OK).then(|| db_error_or(db, status));
    if positional.is_array() {
        let length = length_of(positional);
        if length > count as usize {
            return Some(error_value(25, "column index out of range"));
        }
        for position in 0..length {
            let item = positional.get_property(&position.to_string());
            if let Some(error) = check(bind(statement, position as c_int + 1, &item)) {
                return Some(error);
            }
        }
    }
    if named.is_array() {
        for pair in 0..length_of(named) {
            let pair = named.get_property(&pair.to_string());
            let key = pair.get_property("0").to_js_string();
            let index = if let Ok(position) = key.parse::<c_int>() {
                position
            } else {
                let Ok(name) = CString::new(key.as_str()) else {
                    continue;
                };
                unsafe { sqlite3_bind_parameter_index(statement, name.as_ptr()) }
            };
            if index <= 0 || index > count {
                return Some(error_value(25, "column index out of range"));
            }
            if let Some(error) = check(bind(statement, index, &pair.get_property("1"))) {
                return Some(error);
            }
        }
    }
    None
}

fn db_error_or(db: *mut Sqlite3, status: c_int) -> Value {
    let message = last_message(db);
    if message.is_empty() || message == "not an error" {
        let text = unsafe { sqlite3_errstr(status) };
        let message = if text.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        return error_value(status, &message);
    }
    error_value(status, &message)
}

fn column_value(statement: *mut Sqlite3Stmt, column: c_int) -> Value {
    unsafe {
        match sqlite3_column_type(statement, column) {
            SQLITE_INTEGER => Value::Number(sqlite3_column_int64(statement, column) as f64),
            SQLITE_FLOAT => Value::Number(sqlite3_column_double(statement, column)),
            SQLITE_TEXT => {
                let text = sqlite3_column_text(statement, column);
                let bytes = sqlite3_column_bytes(statement, column).max(0) as usize;
                if text.is_null() {
                    Value::from("")
                } else {
                    let slice = std::slice::from_raw_parts(text, bytes);
                    Value::from(String::from_utf8_lossy(slice).into_owned())
                }
            }
            SQLITE_BLOB => {
                let blob = sqlite3_column_blob(statement, column);
                let bytes = sqlite3_column_bytes(statement, column).max(0) as usize;
                let data = if blob.is_null() {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(blob.cast::<u8>(), bytes).to_vec()
                };
                // The glue wraps it in a Buffer.
                object(vec![("blob", w3cos_core::binary::array_buffer_value(data))])
            }
            _ => Value::Null,
        }
    }
}

/// Prepare the first statement of `sql`, bind `params`, step it to the end.
/// Returns `{ columns, rows, lastID, changes }` (`rows` as arrays, in column
/// order; the glue builds row objects).
fn query(db: *mut Sqlite3, sql: &str, positional: &Value, named: &Value, want_rows: bool) -> Value {
    let Ok(c_sql) = CString::new(sql) else {
        return misuse("SQL contains a NUL byte");
    };
    let mut raw = ptr::null_mut();
    let status = unsafe { sqlite3_prepare_v2(db, c_sql.as_ptr(), -1, &mut raw, ptr::null_mut()) };
    if status != SQLITE_OK {
        return db_error(db);
    }
    if raw.is_null() {
        // Empty SQL (only whitespace or comments).
        return object(vec![
            ("columns", Value::array(vec![])),
            ("rows", Value::array(vec![])),
            ("lastID", Value::Number(0.0)),
            ("changes", Value::Number(0.0)),
        ]);
    }
    let statement = Statement(raw);
    if let Some(error) = bind_params(db, statement.0, positional, named) {
        return error;
    }
    let count = unsafe { sqlite3_column_count(statement.0) };
    let columns: Vec<Value> = (0..count)
        .map(|column| {
            let name = unsafe { sqlite3_column_name(statement.0, column) };
            if name.is_null() {
                Value::from("")
            } else {
                Value::from(
                    unsafe { CStr::from_ptr(name) }
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        })
        .collect();
    let mut rows = Vec::new();
    loop {
        match unsafe { sqlite3_step(statement.0) } {
            SQLITE_ROW => {
                if want_rows {
                    rows.push(Value::array(
                        (0..count)
                            .map(|column| column_value(statement.0, column))
                            .collect(),
                    ));
                }
            }
            SQLITE_DONE => break,
            status => return db_error_or(db, status),
        }
    }
    drop(statement);
    object(vec![
        ("columns", Value::array(columns)),
        ("rows", Value::array(rows)),
        (
            "lastID",
            Value::Number(unsafe { sqlite3_last_insert_rowid(db) } as f64),
        ),
        (
            "changes",
            Value::Number(f64::from(unsafe { sqlite3_changes(db) })),
        ),
    ])
}
