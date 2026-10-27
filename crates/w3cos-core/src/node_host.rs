//! Native host functions behind `w3cos-node`'s `fs` glue.
//!
//! The glue reaches them through the `__w3cos_node_host` global (one object
//! per page). Every function is synchronous and throws a Node-shaped error
//! (`code`, `errno`, `syscall`, `path`, `dest`) on failure; the glue builds
//! the callback and promise APIs on top.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Value;

thread_local! {
    static HOST_VALUE: RefCell<Option<(u32, Value)>> = const { RefCell::new(None) };
    static FILES: RefCell<HashMap<i64, File>> = RefCell::new(HashMap::new());
    static NEXT_FD: std::cell::Cell<i64> = const { std::cell::Cell::new(1000) };
}

/// The `__w3cos_node_host` global.
pub fn node_host_value() -> Value {
    let epoch = crate::page_arena::current_epoch();
    let cached = HOST_VALUE.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|(cached_epoch, _)| *cached_epoch == epoch)
            .map(|(_, value)| value.clone())
    });
    if let Some(value) = cached {
        return value;
    }
    let value = build_host();
    let retired = HOST_VALUE.with(|cell| cell.replace(Some((epoch, value.clone()))));
    drop(retired);
    value
}

pub(crate) fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn path_arg(args: &[Value], index: usize) -> String {
    arg(args, index).to_js_string()
}

fn code_for(error: &io::Error) -> (&'static str, &'static str) {
    if let Some(raw) = error.raw_os_error() {
        if let Some(found) = code_for_errno(raw) {
            return found;
        }
    }
    match error.kind() {
        io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        io::ErrorKind::DirectoryNotEmpty => ("ENOTEMPTY", "directory not empty"),
        io::ErrorKind::InvalidInput => ("EINVAL", "invalid argument"),
        io::ErrorKind::ConnectionRefused => ("ECONNREFUSED", "connection refused"),
        io::ErrorKind::ConnectionReset => ("ECONNRESET", "connection reset by peer"),
        io::ErrorKind::AddrInUse => ("EADDRINUSE", "address already in use"),
        io::ErrorKind::BrokenPipe => ("EPIPE", "broken pipe"),
        io::ErrorKind::TimedOut => ("ETIMEDOUT", "connection timed out"),
        _ => ("EIO", "i/o error"),
    }
}

#[cfg(target_os = "linux")]
fn code_for_errno(raw: i32) -> Option<(&'static str, &'static str)> {
    Some(match raw {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        9 => ("EBADF", "bad file descriptor"),
        13 => ("EACCES", "permission denied"),
        17 => ("EEXIST", "file already exists"),
        18 => ("EXDEV", "cross-device link not permitted"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        22 => ("EINVAL", "invalid argument"),
        24 => ("EMFILE", "too many open files"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        36 => ("ENAMETOOLONG", "name too long"),
        39 => ("ENOTEMPTY", "directory not empty"),
        32 => ("EPIPE", "broken pipe"),
        40 => ("ELOOP", "too many symbolic links encountered"),
        98 => ("EADDRINUSE", "address already in use"),
        104 => ("ECONNRESET", "connection reset by peer"),
        110 => ("ETIMEDOUT", "connection timed out"),
        111 => ("ECONNREFUSED", "connection refused"),
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn code_for_errno(raw: i32) -> Option<(&'static str, &'static str)> {
    Some(match raw {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        9 => ("EBADF", "bad file descriptor"),
        13 => ("EACCES", "permission denied"),
        17 => ("EEXIST", "file already exists"),
        18 => ("EXDEV", "cross-device link not permitted"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        22 => ("EINVAL", "invalid argument"),
        24 => ("EMFILE", "too many open files"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        62 => ("ELOOP", "too many symbolic links encountered"),
        63 => ("ENAMETOOLONG", "name too long"),
        66 => ("ENOTEMPTY", "directory not empty"),
        32 => ("EPIPE", "broken pipe"),
        48 => ("EADDRINUSE", "address already in use"),
        54 => ("ECONNRESET", "connection reset by peer"),
        60 => ("ETIMEDOUT", "connection timed out"),
        61 => ("ECONNREFUSED", "connection refused"),
        _ => return None,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn code_for_errno(_raw: i32) -> Option<(&'static str, &'static str)> {
    None
}

/// Throw `error` as Node does: `ENOENT: no such file or directory, open '/x'`.
fn throw_io(error: io::Error, syscall: &str, path: Option<&str>, dest: Option<&str>) -> ! {
    crate::throw_value(node_error(&error, syscall, path, dest))
}

pub(crate) fn node_error(
    error: &io::Error,
    syscall: &str,
    path: Option<&str>,
    dest: Option<&str>,
) -> Value {
    let (code, description) = code_for(error);
    let mut message = format!("{code}: {description}, {syscall}");
    if let Some(path) = path {
        message.push_str(&format!(" '{path}'"));
    }
    if let Some(dest) = dest {
        message.push_str(&format!(" -> '{dest}'"));
    }
    let value = crate::builtins::error_instance("Error", vec![Value::from(message)]);
    let errno = error.raw_os_error().map_or(-5.0, |raw| -f64::from(raw));
    value.set_property("errno", Value::Number(errno));
    value.set_property("code", Value::from(code));
    value.set_property("syscall", Value::from(syscall));
    if let Some(path) = path {
        value.set_property("path", Value::from(path));
    }
    if let Some(dest) = dest {
        value.set_property("dest", Value::from(dest));
    }
    value
}

/// An OS error with `raw` errno where errno values are POSIX-stable.
fn errno_error(raw: i32, kind: io::ErrorKind) -> io::Error {
    if cfg!(unix) {
        io::Error::from_raw_os_error(raw)
    } else {
        io::Error::from(kind)
    }
}

fn already_exists() -> io::Error {
    errno_error(17, io::ErrorKind::AlreadyExists)
}

fn custom_error(code: &'static str, raw: i32) -> io::Error {
    let _ = code;
    io::Error::from_raw_os_error(raw)
}

/// Bytes of a string (UTF-8), ArrayBuffer or ArrayBuffer view.
pub(crate) fn data_bytes(value: &Value) -> Vec<u8> {
    if let Some(text) = value.as_js_string() {
        return text.as_str().as_bytes().to_vec();
    }
    crate::binary::bytes_of(value).unwrap_or_else(|| value.to_js_string().into_bytes())
}

fn millis(time: io::Result<SystemTime>) -> f64 {
    time.ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0)
}

fn stats_value(metadata: &fs::Metadata) -> Value {
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        3.0
    } else if file_type.is_dir() {
        2.0
    } else if file_type.is_file() {
        1.0
    } else {
        0.0
    };
    let mut props: HashMap<String, Value> = HashMap::from([
        ("kind".into(), Value::Number(kind)),
        ("size".into(), Value::Number(metadata.len() as f64)),
        ("atimeMs".into(), Value::Number(millis(metadata.accessed()))),
        ("mtimeMs".into(), Value::Number(millis(metadata.modified()))),
        (
            "birthtimeMs".into(),
            Value::Number(millis(metadata.created())),
        ),
    ]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime = metadata.ctime() as f64 * 1000.0 + metadata.ctime_nsec() as f64 / 1e6;
        for (key, value) in [
            ("dev", metadata.dev() as f64),
            ("ino", metadata.ino() as f64),
            ("mode", f64::from(metadata.mode())),
            ("nlink", metadata.nlink() as f64),
            ("uid", f64::from(metadata.uid())),
            ("gid", f64::from(metadata.gid())),
            ("rdev", metadata.rdev() as f64),
            ("blksize", metadata.blksize() as f64),
            ("blocks", metadata.blocks() as f64),
            ("ctimeMs", ctime),
        ] {
            props.insert(key.into(), Value::Number(value));
        }
    }
    #[cfg(not(unix))]
    {
        let type_bits = match kind as u32 {
            2 => 0o040000,
            3 => 0o120000,
            _ => 0o100000,
        };
        let perm = if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        };
        for (key, value) in [
            ("dev", 0.0),
            ("ino", 0.0),
            ("mode", f64::from(type_bits | perm)),
            ("nlink", 1.0),
            ("uid", 0.0),
            ("gid", 0.0),
            ("rdev", 0.0),
            ("blksize", 4096.0),
            ("blocks", (metadata.len() as f64 / 512.0).ceil()),
            ("ctimeMs", millis(metadata.modified())),
        ] {
            props.insert(key.into(), Value::Number(value));
        }
    }
    Value::object(props)
}

fn with_file<T>(fd: i64, syscall: &str, f: impl FnOnce(&mut File) -> io::Result<T>) -> T {
    let result = FILES.with(|files| match files.borrow_mut().get_mut(&fd) {
        Some(file) => f(file),
        None => Err(custom_error("EBADF", 9)),
    });
    result.unwrap_or_else(|error| throw_io(error, syscall, None, None))
}

fn open_options(flags: &str) -> Option<OpenOptions> {
    let mut options = OpenOptions::new();
    match flags {
        "r" | "rs" | "sr" => options.read(true),
        "r+" | "rs+" | "sr+" => options.read(true).write(true),
        "w" => options.write(true).create(true).truncate(true),
        "wx" | "xw" => options.write(true).create_new(true),
        "w+" => options.read(true).write(true).create(true).truncate(true),
        "wx+" | "xw+" => options.read(true).write(true).create_new(true),
        "a" | "as" | "sa" => options.append(true).create(true),
        "ax" | "xa" => options.append(true).create_new(true),
        "a+" | "as+" | "sa+" => options.read(true).append(true).create(true),
        "ax+" | "xa+" => options.read(true).append(true).create_new(true),
        _ => return None,
    };
    Some(options)
}

/// `fs.open(path, flags)` with numeric flags built from `fs.constants`
/// (Linux values, as published by node:fs): `O_RDONLY | O_NONBLOCK`, ...
fn open_options_from_bits(bits: u32) -> OpenOptions {
    const O_WRONLY: u32 = 1;
    const O_RDWR: u32 = 2;
    const O_CREAT: u32 = 64;
    const O_EXCL: u32 = 128;
    const O_TRUNC: u32 = 512;
    const O_APPEND: u32 = 1024;
    let mut options = OpenOptions::new();
    match bits & 3 {
        O_WRONLY => options.write(true),
        O_RDWR => options.read(true).write(true),
        _ => options.read(true),
    };
    if bits & O_APPEND != 0 {
        options.append(true);
    }
    if bits & O_CREAT != 0 {
        if bits & O_EXCL != 0 {
            options.create_new(true);
        } else {
            options.create(true);
        }
    }
    if bits & O_TRUNC != 0 {
        options.truncate(true);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK, O_SYNC, O_DSYNC, O_NOFOLLOW, O_NOATIME, O_DIRECTORY, ...
        let rest = bits & !(3 | O_CREAT | O_EXCL | O_TRUNC | O_APPEND);
        if rest != 0 {
            options.custom_flags(rest as i32);
        }
    }
    options
}

fn open_options_for(flags: &Value) -> Option<OpenOptions> {
    if flags.is_undefined() || flags.is_null() {
        return open_options("r");
    }
    if let Some(bits) = flags.as_number() {
        return (bits.is_finite() && bits >= 0.0).then(|| open_options_from_bits(bits as u32));
    }
    open_options(&flags.to_js_string())
}

/// Writes to the process's stdio descriptors (`fs.writeSync(1, ...)`),
/// which node:fs accepts without an `open`.
fn stdio_write(fd: i64, bytes: &[u8]) -> Option<io::Result<usize>> {
    if FILES.with(|files| files.borrow().contains_key(&fd)) {
        return None;
    }
    let result = match fd {
        1 => {
            let mut out = io::stdout().lock();
            out.write_all(bytes).and_then(|()| out.flush())
        }
        2 => {
            let mut err = io::stderr().lock();
            err.write_all(bytes).and_then(|()| err.flush())
        }
        _ => return None,
    };
    Some(result.map(|()| bytes.len()))
}

fn is_unopened_stdio(fd: i64) -> bool {
    (0..=2).contains(&fd) && !FILES.with(|files| files.borrow().contains_key(&fd))
}

#[cfg(unix)]
fn apply_mode(options: &mut OpenOptions, mode: Option<u32>) {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(mode) = mode {
        options.mode(mode);
    }
}

#[cfg(not(unix))]
fn apply_mode(_options: &mut OpenOptions, _mode: Option<u32>) {}

fn mode_arg(value: &Value) -> Option<u32> {
    if value.is_undefined() || value.is_null() {
        return None;
    }
    if let Some(text) = value.as_js_string() {
        return u32::from_str_radix(&text, 8).ok();
    }
    let number = value.to_number();
    number.is_finite().then_some(number as u32)
}

fn register_fd(file: File) -> i64 {
    #[cfg(unix)]
    let fd = {
        use std::os::unix::io::AsRawFd;
        i64::from(file.as_raw_fd())
    };
    #[cfg(not(unix))]
    let fd = NEXT_FD.with(|next| {
        let fd = next.get();
        next.set(fd + 1);
        fd
    });
    FILES.with(|files| files.borrow_mut().insert(fd, file));
    fd
}

fn set_permissions(path: &str, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(mode & 0o200 == 0);
        fs::set_permissions(path, permissions)
    }
}

fn copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn random_suffix() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = crate::node_host::random_u64();
    (0..6)
        .map(|_| {
            let index = (seed % CHARS.len() as u64) as usize;
            seed /= CHARS.len() as u64;
            CHARS[index] as char
        })
        .collect()
}

/// 64 bits from the OS (`/dev/urandom`), falling back to a clock mix.
pub(crate) fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    fill_random(&mut bytes);
    u64::from_le_bytes(bytes)
}

/// Fill `buffer` with OS randomness.
pub(crate) fn fill_random(buffer: &mut [u8]) {
    if File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(buffer))
        .is_ok()
    {
        return;
    }
    // Non-unix fallback: SplitMix64 over the clock and a counter.
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    STATE.with(|state| {
        let mut x = state.get()
            ^ SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
        for byte in buffer.iter_mut() {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *byte = (z ^ (z >> 31)) as u8;
        }
        state.set(x);
    });
}

/// Host functions added by crates above core (the runtime's SQLite and file
/// watcher bridges), installed on every `__w3cos_node_host` built afterwards.
static EXTENSIONS: std::sync::Mutex<Vec<fn(&Value)>> = std::sync::Mutex::new(Vec::new());

/// Register `install` to add functions to `__w3cos_node_host` (idempotent).
pub fn register_extension(install: fn(&Value)) {
    {
        let mut extensions = EXTENSIONS.lock().unwrap();
        if !extensions
            .iter()
            .any(|known| std::ptr::fn_addr_eq(*known, install))
        {
            extensions.push(install);
        }
    }
    // A host object this thread built before the call gets it too
    // (defining the functions again is harmless).
    let current = HOST_VALUE.with(|cell| cell.borrow().as_ref().map(|(_, value)| value.clone()));
    if let Some(host) = current {
        install(&host);
    }
}

/// Add a host function `name` to `host` (for `register_extension` installers).
pub fn define(host: &Value, name: &str, f: impl Fn(Vec<Value>) -> Value + 'static) {
    host.set_property(name, crate::named_function(name, 0, move |_, args| f(args)));
}

fn build_host() -> Value {
    let host = Value::object(HashMap::new());
    define(&host, "readFile", |args| {
        let path = path_arg(&args, 0);
        match fs::read(&path) {
            Ok(bytes) => crate::binary::array_buffer_value(bytes),
            Err(error) => throw_io(error, "open", Some(&path), None),
        }
    });
    define(&host, "readFileUtf8", |args| {
        let path = path_arg(&args, 0);
        match fs::read(&path) {
            Ok(bytes) => Value::from(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) => throw_io(error, "open", Some(&path), None),
        }
    });
    define(&host, "writeFile", |args| {
        let path = path_arg(&args, 0);
        let bytes = data_bytes(&arg(&args, 1));
        let flag = arg(&args, 2);
        let options = if flag.is_undefined() || flag.is_null() {
            open_options("w")
        } else {
            open_options_for(&flag)
        };
        let Some(mut options) = options else {
            throw_io(custom_error("EINVAL", 22), "open", Some(&path), None);
        };
        apply_mode(&mut options, mode_arg(&arg(&args, 3)));
        let result = options
            .open(&path)
            .and_then(|mut file| file.write_all(&bytes));
        if let Err(error) = result {
            throw_io(error, "open", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "stat", |args| {
        let path = path_arg(&args, 0);
        let follow = !arg(&args, 1).is_truthy_false();
        let result = if follow {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        match result {
            Ok(metadata) => stats_value(&metadata),
            Err(error) if arg(&args, 2).to_bool() && error.kind() == io::ErrorKind::NotFound => {
                Value::Undefined
            }
            Err(error) => throw_io(
                error,
                if follow { "stat" } else { "lstat" },
                Some(&path),
                None,
            ),
        }
    });
    define(&host, "fstat", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        let metadata = with_file(fd, "fstat", |file| file.metadata());
        stats_value(&metadata)
    });
    define(&host, "readdir", |args| {
        let path = path_arg(&args, 0);
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => throw_io(error, "scandir", Some(&path), None),
        };
        let mut items: Vec<(String, f64)> = Vec::new();
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| throw_io(error, "scandir", Some(&path), None));
            let kind = entry.file_type().map_or(0.0, |file_type| {
                if file_type.is_symlink() {
                    3.0
                } else if file_type.is_dir() {
                    2.0
                } else if file_type.is_file() {
                    1.0
                } else {
                    0.0
                }
            });
            items.push((entry.file_name().to_string_lossy().into_owned(), kind));
        }
        // Node returns entries sorted (libuv scandir sorts).
        items.sort_by(|a, b| a.0.cmp(&b.0));
        Value::array(
            items
                .into_iter()
                .map(|(name, kind)| Value::array(vec![Value::from(name), Value::Number(kind)]))
                .collect(),
        )
    });
    define(&host, "mkdir", |args| {
        let path = path_arg(&args, 0);
        let recursive = arg(&args, 1).to_bool();
        if recursive {
            // Node returns the first directory created (or undefined).
            let mut first: Option<String> = None;
            let mut probe = Path::new(&path).to_path_buf();
            loop {
                if probe.exists() {
                    break;
                }
                first = Some(probe.to_string_lossy().into_owned());
                if !probe.pop() || probe.as_os_str().is_empty() {
                    break;
                }
            }
            if let Err(error) = fs::create_dir_all(&path) {
                throw_io(error, "mkdir", Some(&path), None);
            }
            return first.map_or(Value::Undefined, Value::from);
        }
        if let Err(error) = fs::create_dir(&path) {
            throw_io(error, "mkdir", Some(&path), None);
        }
        if let Some(mode) = mode_arg(&arg(&args, 2)) {
            let _ = set_permissions(&path, mode);
        }
        Value::Undefined
    });
    define(&host, "rmdir", |args| {
        let path = path_arg(&args, 0);
        let result = if arg(&args, 1).to_bool() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_dir(&path)
        };
        if let Err(error) = result {
            throw_io(error, "rmdir", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "rm", |args| {
        let path = path_arg(&args, 0);
        let recursive = arg(&args, 1).to_bool();
        let force = arg(&args, 2).to_bool();
        let result = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                if recursive {
                    fs::remove_dir_all(&path)
                } else {
                    let error =
                        node_error(&io::Error::from_raw_os_error(21), "rm", Some(&path), None);
                    error.set_property("code", Value::from("ERR_FS_EISDIR"));
                    error.set_property(
                        "message",
                        Value::from(format!(
                            "Path is a directory: rm returned EISDIR (is a directory) {path}"
                        )),
                    );
                    crate::throw_value(error);
                }
            }
            Ok(_) => fs::remove_file(&path),
            Err(error) if force && error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            throw_io(error, "rm", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "unlink", |args| {
        let path = path_arg(&args, 0);
        if let Err(error) = fs::remove_file(&path) {
            throw_io(error, "unlink", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "rename", |args| {
        let (from, to) = (path_arg(&args, 0), path_arg(&args, 1));
        if let Err(error) = fs::rename(&from, &to) {
            throw_io(error, "rename", Some(&from), Some(&to));
        }
        Value::Undefined
    });
    define(&host, "copyFile", |args| {
        let (from, to) = (path_arg(&args, 0), path_arg(&args, 1));
        if arg(&args, 2).to_bool() && Path::new(&to).exists() {
            throw_io(already_exists(), "copyfile", Some(&from), Some(&to));
        }
        if let Err(error) = fs::copy(&from, &to) {
            throw_io(error, "copyfile", Some(&from), Some(&to));
        }
        Value::Undefined
    });
    define(&host, "cp", |args| {
        let (from, to) = (path_arg(&args, 0), path_arg(&args, 1));
        let source = Path::new(&from);
        let result = if source.is_dir() {
            if arg(&args, 2).to_bool() {
                copy_dir_all(source, Path::new(&to))
            } else {
                Err(io::Error::from_raw_os_error(21))
            }
        } else {
            fs::copy(&from, &to).map(|_| ())
        };
        if let Err(error) = result {
            throw_io(error, "cp", Some(&from), Some(&to));
        }
        Value::Undefined
    });
    define(&host, "realpath", |args| {
        let path = path_arg(&args, 0);
        match fs::canonicalize(&path) {
            Ok(real) => Value::from(real.to_string_lossy().into_owned()),
            Err(error) => throw_io(error, "realpath", Some(&path), None),
        }
    });
    define(&host, "readlink", |args| {
        let path = path_arg(&args, 0);
        match fs::read_link(&path) {
            Ok(target) => Value::from(target.to_string_lossy().into_owned()),
            Err(error) => throw_io(error, "readlink", Some(&path), None),
        }
    });
    define(&host, "symlink", |args| {
        let (target, path) = (path_arg(&args, 0), path_arg(&args, 1));
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(&target, &path);
        #[cfg(windows)]
        let result = if Path::new(&target).is_dir() {
            std::os::windows::fs::symlink_dir(&target, &path)
        } else {
            std::os::windows::fs::symlink_file(&target, &path)
        };
        #[cfg(not(any(unix, windows)))]
        let result: io::Result<()> = Err(io::Error::from(io::ErrorKind::Unsupported));
        if let Err(error) = result {
            throw_io(error, "symlink", Some(&target), Some(&path));
        }
        Value::Undefined
    });
    define(&host, "link", |args| {
        let (existing, path) = (path_arg(&args, 0), path_arg(&args, 1));
        if let Err(error) = fs::hard_link(&existing, &path) {
            throw_io(error, "link", Some(&existing), Some(&path));
        }
        Value::Undefined
    });
    define(&host, "chmod", |args| {
        let path = path_arg(&args, 0);
        let mode = mode_arg(&arg(&args, 1)).unwrap_or(0o644);
        if let Err(error) = set_permissions(&path, mode) {
            throw_io(error, "chmod", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "utimes", |args| {
        let path = path_arg(&args, 0);
        let to_time = |value: Value| {
            let seconds = value.to_number();
            UNIX_EPOCH
                + Duration::from_secs_f64(if seconds.is_finite() {
                    seconds.max(0.0)
                } else {
                    0.0
                })
        };
        let times = fs::FileTimes::new()
            .set_accessed(to_time(arg(&args, 1)))
            .set_modified(to_time(arg(&args, 2)));
        let result = OpenOptions::new()
            .write(true)
            .open(&path)
            .or_else(|_| File::open(&path))
            .and_then(|file| file.set_times(times));
        if let Err(error) = result {
            throw_io(error, "utime", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "access", |args| {
        let path = path_arg(&args, 0);
        let mode = arg(&args, 1).to_number();
        let mode = if mode.is_finite() { mode as u32 } else { 0 };
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|error| throw_io(error, "access", Some(&path), None));
        let denied = (mode & 2 != 0 && metadata.permissions().readonly()) || {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                mode & 1 != 0 && metadata.permissions().mode() & 0o111 == 0
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        if denied {
            throw_io(
                errno_error(13, io::ErrorKind::PermissionDenied),
                "access",
                Some(&path),
                None,
            );
        }
        Value::Undefined
    });
    define(&host, "truncate", |args| {
        let path = path_arg(&args, 0);
        let length = arg(&args, 1).to_number().max(0.0) as u64;
        let result = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_len(length));
        if let Err(error) = result {
            throw_io(error, "open", Some(&path), None);
        }
        Value::Undefined
    });
    define(&host, "mkdtemp", |args| {
        let prefix = path_arg(&args, 0);
        for _ in 0..32 {
            let candidate = format!("{prefix}{}", random_suffix());
            match fs::create_dir(&candidate) {
                Ok(()) => return Value::from(candidate),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => throw_io(error, "mkdtemp", Some(&format!("{prefix}XXXXXX")), None),
            }
        }
        throw_io(already_exists(), "mkdtemp", Some(&prefix), None)
    });
    define(&host, "open", |args| {
        let path = path_arg(&args, 0);
        let Some(mut options) = open_options_for(&arg(&args, 1)) else {
            throw_io(custom_error("EINVAL", 22), "open", Some(&path), None);
        };
        apply_mode(&mut options, mode_arg(&arg(&args, 2)));
        match options.open(&path) {
            Ok(file) => Value::Number(register_fd(file) as f64),
            Err(error) => throw_io(error, "open", Some(&path), None),
        }
    });
    define(&host, "close", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        let removed = FILES.with(|files| files.borrow_mut().remove(&fd));
        if removed.is_none() {
            throw_io(custom_error("EBADF", 9), "close", None, None);
        }
        Value::Undefined
    });
    define(&host, "read", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        let length = arg(&args, 1).to_number().max(0.0) as usize;
        let position = arg(&args, 2);
        let bytes = with_file(fd, "read", |file| {
            if let Some(position) = position.as_number().filter(|p| *p >= 0.0) {
                file.seek(SeekFrom::Start(position as u64))?;
            }
            let mut buffer = vec![0u8; length];
            let mut filled = 0;
            while filled < length {
                let count = file.read(&mut buffer[filled..])?;
                if count == 0 {
                    break;
                }
                filled += count;
            }
            buffer.truncate(filled);
            Ok(buffer)
        });
        crate::binary::array_buffer_value(bytes)
    });
    define(&host, "write", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        let bytes = data_bytes(&arg(&args, 1));
        let position = arg(&args, 2);
        if let Some(result) = stdio_write(fd, &bytes) {
            let written = result.unwrap_or_else(|error| throw_io(error, "write", None, None));
            return Value::Number(written as f64);
        }
        let written = with_file(fd, "write", |file| {
            if let Some(position) = position.as_number().filter(|p| *p >= 0.0) {
                file.seek(SeekFrom::Start(position as u64))?;
            }
            file.write_all(&bytes)?;
            Ok(bytes.len())
        });
        Value::Number(written as f64)
    });
    define(&host, "fsync", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        if is_unopened_stdio(fd) {
            return Value::Undefined;
        }
        with_file(fd, "fsync", |file| file.sync_all());
        Value::Undefined
    });
    define(&host, "fdatasync", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        if is_unopened_stdio(fd) {
            return Value::Undefined;
        }
        with_file(fd, "fdatasync", |file| file.sync_data());
        Value::Undefined
    });
    define(&host, "ftruncate", |args| {
        let fd = arg(&args, 0).to_number() as i64;
        let length = arg(&args, 1).to_number().max(0.0) as u64;
        with_file(fd, "ftruncate", |file| file.set_len(length));
        Value::Undefined
    });
    define(&host, "randomBytes", |args| {
        let length = arg(&args, 0).to_number().max(0.0) as usize;
        let mut bytes = vec![0u8; length];
        fill_random(&mut bytes);
        crate::binary::array_buffer_value(bytes)
    });
    define(&host, "hash", |args| {
        let algorithm = arg(&args, 0).to_js_string();
        match digest(&algorithm, &data_bytes(&arg(&args, 1))) {
            Some(bytes) => crate::binary::array_buffer_value(bytes),
            None => Value::Undefined,
        }
    });
    define(&host, "hmac", |args| {
        let algorithm = arg(&args, 0).to_js_string();
        let key = data_bytes(&arg(&args, 1));
        match hmac_digest(&algorithm, &key, &data_bytes(&arg(&args, 2))) {
            Some(bytes) => crate::binary::array_buffer_value(bytes),
            None => Value::Undefined,
        }
    });
    define(&host, "pbkdf2", |args| {
        let password = data_bytes(&arg(&args, 0));
        let salt = data_bytes(&arg(&args, 1));
        let iterations = arg(&args, 2).to_number().max(1.0) as u32;
        let length = arg(&args, 3).to_number().max(0.0) as usize;
        let algorithm = arg(&args, 4).to_js_string();
        match pbkdf2(&algorithm, &password, &salt, iterations, length) {
            Some(bytes) => crate::binary::array_buffer_value(bytes),
            None => Value::Undefined,
        }
    });
    define(&host, "zlib", |args| {
        let op = arg(&args, 0).to_js_string();
        let data = data_bytes(&arg(&args, 1));
        let level = arg(&args, 2);
        let level = if level.is_undefined() || level.to_number() < 0.0 {
            6
        } else {
            level.to_number().min(9.0) as u32
        };
        match zlib(&op, &data, level) {
            Ok(bytes) => crate::binary::array_buffer_value(bytes),
            Err((code, errno, message)) => {
                let error = crate::builtins::error_instance("Error", vec![Value::from(message)]);
                error.set_property("errno", Value::Number(f64::from(errno)));
                error.set_property("code", Value::from(code));
                crate::throw_value(error)
            }
        }
    });
    // compiledModule(path) -> whether the path is a module of the compiled graph.
    // requireCompiled(path) -> module.exports (CommonJS) / namespace (ESM)
    // of a module linked into the app, or undefined when the path is not in
    // the compiled graph. Lets `require(absolutePath)` reach modules the app
    // declares for run-time loading (`package.json` `"w3cos": { "modules" }`).
    define(&host, "compiledModule", |args| {
        Value::Bool(crate::module_registry::contains(&arg(&args, 0).to_js_string()))
    });
    define(&host, "requireCompiled", |args| {
        require_compiled(&arg(&args, 0).to_js_string()).unwrap_or(Value::Undefined)
    });
    crate::node_host_io::install(&host);
    crate::node_workers::install(&host);
    for install in EXTENSIONS.lock().unwrap().iter() {
        install(&host);
    }
    host
}

/// Evaluate a compiled module by path for a run-time `require()`.
///
/// Lazily linked modules (declared run-time roots) evaluate their static
/// closure through the dynamic-import adapter; statically linked modules are
/// evaluated (or already were) in place. A CommonJS file's facade default is
/// its `module.exports`; an ES module yields its namespace, as in Node's
/// `require(esm)`. An evaluation error is rethrown.
pub fn require_compiled(path: &str) -> Option<Value> {
    use crate::module_registry;
    if !module_registry::contains(path) {
        return None;
    }
    let loaded = crate::host_modules::dynamic_import(Value::from(path), Value::from(path));
    if let Some(crate::promise::PromiseStatus::Rejected(_)) = crate::promise::status(&loaded) {
        // Not a lazily linked root: mark the probe handled and evaluate in place.
        let _ = crate::intrinsics::call_method(
            &loaded,
            &Value::from("catch"),
            vec![Value::function(|_, _| Value::Undefined)],
        );
    }
    if let Some(crate::promise::PromiseStatus::Rejected(error)) =
        module_registry::evaluate(path).as_ref().and_then(crate::promise::status)
    {
        crate::throw_value(error);
    }
    let commonjs = module_registry::contains(&format!("{path}.w3cos-cjs.js"));
    if commonjs {
        if let Some(binding) = module_registry::export(path, "default") {
            return Some(binding.read());
        }
    }
    module_registry::namespace(path)
}

/// Canonical digest name for Node's spellings (`SHA256`, `sha-256`, `RSA-SHA1`).
fn digest_name(algorithm: &str) -> Option<&'static str> {
    let lower = algorithm.to_ascii_lowercase();
    let lower = lower.strip_prefix("rsa-").unwrap_or(&lower);
    Some(match lower {
        "md5" => "md5",
        "sha1" | "sha-1" => "sha1",
        "sha224" | "sha-224" => "sha224",
        "sha256" | "sha-256" => "sha256",
        "sha384" | "sha-384" => "sha384",
        "sha512" | "sha-512" => "sha512",
        _ => return None,
    })
}

fn digest(algorithm: &str, data: &[u8]) -> Option<Vec<u8>> {
    use sha2::Digest;
    Some(match digest_name(algorithm)? {
        "md5" => md5::Md5::digest(data).to_vec(),
        "sha1" => sha1::Sha1::digest(data).to_vec(),
        "sha224" => sha2::Sha224::digest(data).to_vec(),
        "sha256" => sha2::Sha256::digest(data).to_vec(),
        "sha384" => sha2::Sha384::digest(data).to_vec(),
        _ => sha2::Sha512::digest(data).to_vec(),
    })
}

fn mac<M: hmac::Mac + hmac::digest::KeyInit>(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <M as hmac::Mac>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_digest(algorithm: &str, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use hmac::Hmac;
    Some(match digest_name(algorithm)? {
        "md5" => mac::<Hmac<md5::Md5>>(key, data),
        "sha1" => mac::<Hmac<sha1::Sha1>>(key, data),
        "sha224" => mac::<Hmac<sha2::Sha224>>(key, data),
        "sha256" => mac::<Hmac<sha2::Sha256>>(key, data),
        "sha384" => mac::<Hmac<sha2::Sha384>>(key, data),
        _ => mac::<Hmac<sha2::Sha512>>(key, data),
    })
}

/// PBKDF2 (RFC 8018) over HMAC-`algorithm`.
fn pbkdf2(
    algorithm: &str,
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    length: usize,
) -> Option<Vec<u8>> {
    digest_name(algorithm)?;
    let mut out = Vec::with_capacity(length);
    let mut block = 1u32;
    while out.len() < length {
        let mut input = salt.to_vec();
        input.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac_digest(algorithm, password, &input)?;
        let mut t = u.clone();
        for _ in 1..iterations {
            u = hmac_digest(algorithm, password, &u)?;
            for (acc, byte) in t.iter_mut().zip(&u) {
                *acc ^= byte;
            }
        }
        out.extend_from_slice(&t);
        block += 1;
    }
    out.truncate(length);
    Some(out)
}

type ZlibError = (&'static str, i32, String);

fn zlib_data_error(error: io::Error, raw: bool) -> ZlibError {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        return ("Z_BUF_ERROR", -5, "unexpected end of file".into());
    }
    let message = if raw {
        "invalid block type"
    } else {
        "incorrect data check"
    };
    ("Z_DATA_ERROR", -3, message.into())
}

fn header_error() -> ZlibError {
    ("Z_DATA_ERROR", -3, "incorrect header check".into())
}

/// RFC 1950 header: CM 8, window <= 32K, FCHECK, no preset dictionary.
fn valid_zlib_header(data: &[u8]) -> bool {
    data.len() < 2
        || (data[0] & 0x0f == 8
            && data[0] >> 4 <= 7
            && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0
            && data[1] & 0x20 == 0)
}

/// One-shot (de)compression for `zlib`'s `*Sync` / callback APIs.
fn zlib(op: &str, data: &[u8], level: u32) -> Result<Vec<u8>, ZlibError> {
    use flate2::Compression;
    use flate2::read::{
        DeflateDecoder, DeflateEncoder, GzDecoder, MultiGzDecoder, ZlibDecoder, ZlibEncoder,
    };
    let level = Compression::new(level);
    let mut out = Vec::new();
    let read = |mut reader: Box<dyn Read + '_>, out: &mut Vec<u8>| reader.read_to_end(out);
    let result = match op {
        "deflate" => read(Box::new(ZlibEncoder::new(data, level)), &mut out),
        "deflateRaw" => read(Box::new(DeflateEncoder::new(data, level)), &mut out),
        "gzip" => read(
            Box::new(
                flate2::GzBuilder::new()
                    .operating_system(3)
                    .read(data, level),
            ),
            &mut out,
        ),
        "inflate" => {
            if !valid_zlib_header(data) {
                return Err(header_error());
            }
            read(Box::new(ZlibDecoder::new(data)), &mut out)
        }
        "inflateRaw" => read(Box::new(DeflateDecoder::new(data)), &mut out),
        "gunzip" => {
            if data.len() >= 2 && data[..2] != [0x1f, 0x8b] {
                return Err(header_error());
            }
            read(Box::new(MultiGzDecoder::new(data)), &mut out)
        }
        "unzip" => {
            if data.starts_with(&[0x1f, 0x8b]) {
                read(Box::new(GzDecoder::new(data)), &mut out)
            } else if !valid_zlib_header(data) {
                return Err(header_error());
            } else {
                read(Box::new(ZlibDecoder::new(data)), &mut out)
            }
        }
        _ => {
            return Err((
                "ERR_INVALID_ARG_VALUE",
                -22,
                format!("unknown zlib op {op}"),
            ));
        }
    };
    match result {
        Ok(_) => Ok(out),
        Err(error) => Err(zlib_data_error(error, op == "inflateRaw")),
    }
}

trait FalseFlag {
    fn is_truthy_false(&self) -> bool;
}

impl FalseFlag for Value {
    /// `false` exactly (undefined means "default true").
    fn is_truthy_false(&self) -> bool {
        self.as_bool() == Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_node_shaped() {
        let error = node_error(
            &io::Error::from_raw_os_error(2),
            "open",
            Some("/nope"),
            None,
        );
        assert_eq!(error.get_property("code").to_js_string(), "ENOENT");
        assert_eq!(error.get_property("syscall").to_js_string(), "open");
        assert_eq!(error.get_property("errno").to_number(), -2.0);
        assert_eq!(
            error.get_property("message").to_js_string(),
            "ENOENT: no such file or directory, open '/nope'"
        );
    }

    #[test]
    fn digests_and_kdfs_match_known_vectors() {
        let hex = |bytes: Vec<u8>| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(digest("SHA-256", b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(hmac_digest("md5", b"key", b"msg").unwrap()),
            "18e3548c59ad40dd03907b7aeee71d67"
        );
        assert_eq!(
            hex(pbkdf2("sha1", b"password", b"salt", 2, 20).unwrap()),
            "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957"
        );
        assert!(digest("nope", b"").is_none());
    }

    #[test]
    fn zlib_round_trips_and_rejects_garbage() {
        for (enc, dec) in [
            ("deflate", "inflate"),
            ("deflateRaw", "inflateRaw"),
            ("gzip", "gunzip"),
        ] {
            let packed = zlib(enc, b"hello hello hello", 6).unwrap();
            assert_eq!(zlib(dec, &packed, 6).unwrap(), b"hello hello hello");
        }
        let gz = zlib("gzip", b"x", 6).unwrap();
        assert_eq!(zlib("unzip", &gz, 6).unwrap(), b"x");
        assert_eq!(
            zlib("inflate", b"garbage", 6).unwrap_err().0,
            "Z_DATA_ERROR"
        );
    }

    #[test]
    fn open_flags_map_like_node() {
        assert!(open_options("r").is_some());
        assert!(open_options("wx+").is_some());
        assert!(open_options("bogus").is_none());
        assert!(open_options_for(&Value::Number(0.0)).is_some());
        assert!(open_options_for(&Value::Number(-1.0)).is_none());
    }

    #[test]
    fn numeric_open_flags_and_stdio_fds_work_like_node() {
        let dir = std::env::temp_dir().join(format!("w3cos-numeric-open-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        // O_WRONLY | O_CREAT | O_TRUNC
        let mut file = open_options_from_bits(1 | 64 | 512).open(&path).unwrap();
        file.write_all(b"hi").unwrap();
        drop(file);
        // O_RDONLY | O_NONBLOCK (DiskFileSystemProvider.openFileForRead)
        let mut text = String::new();
        open_options_from_bits(2048)
            .open(&path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "hi");
        // O_WRONLY | O_CREAT | O_EXCL on an existing file fails.
        assert!(open_options_from_bits(1 | 64 | 128).open(&path).is_err());
        assert_eq!(stdio_write(2, b"").unwrap().unwrap(), 0);
        assert!(stdio_write(7, b"x").is_none());
        assert!(is_unopened_stdio(1));
        let _ = fs::remove_dir_all(&dir);
    }
}
