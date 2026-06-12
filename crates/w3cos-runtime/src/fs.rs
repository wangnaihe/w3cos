use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Metadata about a file or directory.
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Result of a file read operation.
#[derive(Debug, Clone)]
pub struct FileContent {
    pub path: String,
    pub text: String,
    pub ok: bool,
    pub error: String,
}

impl FileContent {
    fn success(path: &str, text: String) -> Self {
        Self {
            path: path.to_string(),
            text,
            ok: true,
            error: String::new(),
        }
    }

    fn error(path: &str, err: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_string(),
            text: String::new(),
            ok: false,
            error: err.to_string(),
        }
    }
}

/// Result of a file write operation.
#[derive(Debug, Clone)]
pub struct WriteResult {
    pub path: String,
    pub ok: bool,
    pub error: String,
    pub bytes_written: u64,
}

impl WriteResult {
    fn success(path: &str, bytes: u64) -> Self {
        Self {
            path: path.to_string(),
            ok: true,
            error: String::new(),
            bytes_written: bytes,
        }
    }

    fn error(path: &str, err: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_string(),
            ok: false,
            error: err.to_string(),
            bytes_written: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// File System Operations
// ---------------------------------------------------------------------------

/// Read a file as UTF-8 text.
pub fn read_text_file(path: &str) -> FileContent {
    match std::fs::read_to_string(path) {
        Ok(text) => FileContent::success(path, text),
        Err(e) => FileContent::error(path, e),
    }
}

/// Read a file as raw bytes, returned as a Vec<u8>.
pub fn read_binary_file(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| e.to_string())
}

/// Write text content to a file (creates or overwrites).
pub fn write_text_file(path: &str, content: &str) -> WriteResult {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return WriteResult::error(path, e);
            }
        }
    }
    match std::fs::write(path, content) {
        Ok(()) => WriteResult::success(path, content.len() as u64),
        Err(e) => WriteResult::error(path, e),
    }
}

/// Append text to a file (creates if not exists).
pub fn append_text_file(path: &str, content: &str) -> WriteResult {
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut file) => match file.write_all(content.as_bytes()) {
            Ok(()) => WriteResult::success(path, content.len() as u64),
            Err(e) => WriteResult::error(path, e),
        },
        Err(e) => WriteResult::error(path, e),
    }
}

/// Check if a file or directory exists.
pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Delete a file.
pub fn remove_file(path: &str) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| e.to_string())
}

/// Delete a directory and all contents.
pub fn remove_dir(path: &str) -> Result<(), String> {
    std::fs::remove_dir_all(path).map_err(|e| e.to_string())
}

/// Create a directory (and parents if needed).
pub fn create_dir(path: &str) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())
}

/// List directory contents.
pub fn read_dir(path: &str) -> Result<Vec<FileInfo>, String> {
    let entries = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        result.push(FileInfo {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }
    result.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
    });
    Ok(result)
}

/// Copy a file from src to dst.
pub fn copy_file(src: &str, dst: &str) -> Result<u64, String> {
    std::fs::copy(src, dst).map_err(|e| e.to_string())
}

/// Rename/move a file or directory.
pub fn rename(src: &str, dst: &str) -> Result<(), String> {
    std::fs::rename(src, dst).map_err(|e| e.to_string())
}

/// Get file metadata.
pub fn stat(path: &str) -> Result<FileInfo, String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(FileInfo {
        name,
        path: path.to_string(),
        is_dir: metadata.is_dir(),
        size: metadata.len(),
    })
}

/// Get the current working directory.
pub fn cwd() -> Result<String, String> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| e.to_string())
}

/// Get the home directory.
pub fn home_dir() -> Option<String> {
    dirs_fallback().map(|p| p.to_string_lossy().to_string())
}

fn dirs_fallback() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Path utilities (w3cos.path namespace)
// ---------------------------------------------------------------------------

pub fn path_join(segments: &[&str]) -> String {
    let mut path = PathBuf::new();
    for seg in segments {
        path.push(seg);
    }
    path.to_string_lossy().to_string()
}

pub fn path_dirname(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub fn path_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub fn path_extname(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default()
}

pub fn path_resolve(path: &str) -> String {
    match std::fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Environment variables (w3cos.env namespace)
// ---------------------------------------------------------------------------

pub fn env_get(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn env_set(name: &str, value: &str) {
    unsafe { std::env::set_var(name, value) };
}

pub fn env_all() -> HashMap<String, String> {
    std::env::vars().collect()
}

// ---------------------------------------------------------------------------
// File watching (w3cos.fs.watch)
// ---------------------------------------------------------------------------

/// A file system change event.
#[derive(Debug, Clone)]
pub struct FsEvent {
    pub path: String,
    pub kind: FsEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEventKind {
    Create,
    Modify,
    Remove,
    Rename,
    Other,
}

/// A handle to an active file watcher. Drop to stop watching.
pub struct FsWatcher {
    _watcher: notify::RecommendedWatcher,
    pub rx: mpsc::Receiver<FsEvent>,
}

impl FsWatcher {
    /// Drain all pending events (non-blocking).
    pub fn try_recv_all(&self) -> Vec<FsEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Block until the next event arrives (or timeout).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<FsEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

/// Watch a path for changes. Returns a `FsWatcher` handle.
///
/// The watcher runs on a background thread. Drop the handle to stop.
pub fn watch(path: &str) -> Result<FsWatcher, String> {
    use notify::{EventKind, RecursiveMode, Watcher};

    let (tx, rx) = mpsc::channel::<FsEvent>();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let kind = match event.kind {
                EventKind::Create(_) => FsEventKind::Create,
                EventKind::Modify(_) => FsEventKind::Modify,
                EventKind::Remove(_) => FsEventKind::Remove,
                EventKind::Any => FsEventKind::Other,
                _ => FsEventKind::Other,
            };
            for p in event.paths {
                let _ = tx.send(FsEvent {
                    path: p.to_string_lossy().to_string(),
                    kind: kind.clone(),
                });
            }
        }
    })
    .map_err(|e| e.to_string())?;

    watcher
        .watch(Path::new(path), RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    Ok(FsWatcher { _watcher: watcher, rx })
}

// ---------------------------------------------------------------------------
// Permissions & ownership (w3cos.fs.chmod / chown)
// ---------------------------------------------------------------------------

/// Set file permissions (Unix mode bits, e.g. 0o644).
pub fn chmod(path: &str, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err("chmod is only supported on Unix".to_string())
    }
}

/// Change file owner and group (Unix only).
pub fn chown(path: &str, uid: u32, gid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use nix::unistd::{Gid, Uid};
        nix::unistd::chown(
            path,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, uid, gid);
        Err("chown is only supported on Unix".to_string())
    }
}

// ---------------------------------------------------------------------------
// Symbolic links (w3cos.fs.symlink / readlink)
// ---------------------------------------------------------------------------

/// Create a symbolic link: `dst` → `src`.
pub fn symlink(src: &str, dst: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dst).map_err(|e| e.to_string())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (src, dst);
        Err("symlink not supported on this platform".to_string())
    }
}

/// Read the target of a symbolic link.
pub fn readlink(path: &str) -> Result<String, String> {
    std::fs::read_link(path)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Truncate (w3cos.fs.truncate)
// ---------------------------------------------------------------------------

/// Truncate a file to the given length in bytes.
pub fn truncate(path: &str, len: u64) -> Result<(), String> {
    use std::fs::OpenOptions;
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_len(len))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Temporary directory (w3cos.fs.mkdtemp)
// ---------------------------------------------------------------------------

/// Create a temporary directory with the given prefix.
/// Returns the path of the created directory.
pub fn mkdtemp(prefix: &str) -> Result<String, String> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map(|d| {
            let path = d.path().to_string_lossy().to_string();
            // Keep the directory alive by leaking the TempDir handle.
            // Caller is responsible for cleanup via remove_dir().
            d.into_path();
            path
        })
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Glob pattern matching (w3cos.fs.glob)
// ---------------------------------------------------------------------------

/// Return all paths matching a glob pattern (e.g. `"/src/**/*.ts"`).
pub fn glob_files(pattern: &str) -> Result<Vec<String>, String> {
    let paths = glob::glob(pattern).map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for entry in paths {
        match entry {
            Ok(p) => result.push(p.to_string_lossy().to_string()),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Extended metadata (w3cos.fs.stat extended)
// ---------------------------------------------------------------------------

/// Extended file metadata including permissions and timestamps.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub mode: u32,
    pub modified_ms: u64,
    pub created_ms: u64,
}

/// Get extended metadata for a path.
pub fn metadata(path: &str) -> Result<FileMetadata, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    };
    #[cfg(not(unix))]
    let mode = 0u32;

    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let created_ms = meta
        .created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    Ok(FileMetadata {
        name,
        path: path.to_string(),
        is_dir: meta.is_dir(),
        is_symlink: meta.file_type().is_symlink(),
        size: meta.len(),
        mode,
        modified_ms,
        created_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_text() {
        let tmp = std::env::temp_dir().join("w3cos_fs_test.txt");
        let path = tmp.to_string_lossy().to_string();

        let wr = write_text_file(&path, "hello w3cos");
        assert!(wr.ok, "write error: {}", wr.error);
        assert_eq!(wr.bytes_written, 11);

        let rd = read_text_file(&path);
        assert!(rd.ok, "read error: {}", rd.error);
        assert_eq!(rd.text, "hello w3cos");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn append_file() {
        let tmp = std::env::temp_dir().join("w3cos_fs_append.txt");
        let path = tmp.to_string_lossy().to_string();

        write_text_file(&path, "first");
        append_text_file(&path, " second");

        let rd = read_text_file(&path);
        assert_eq!(rd.text, "first second");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn dir_operations() {
        let tmp = std::env::temp_dir().join("w3cos_fs_dir_test");
        let dir = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(create_dir(&dir).is_ok());
        assert!(exists(&dir));

        let file_path = format!("{}/test.txt", dir);
        write_text_file(&file_path, "content");

        let entries = read_dir(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "test.txt");

        assert!(remove_dir(&dir).is_ok());
        assert!(!exists(&dir));
    }

    #[test]
    fn path_utils() {
        assert_eq!(path_join(&["foo", "bar", "baz.txt"]), "foo/bar/baz.txt");
        assert_eq!(path_dirname("/usr/local/bin"), "/usr/local");
        assert_eq!(path_basename("/usr/local/bin"), "bin");
        assert_eq!(path_extname("file.txt"), ".txt");
        assert_eq!(path_extname("noext"), "");
    }

    #[test]
    fn env_ops() {
        env_set("W3COS_TEST_VAR", "hello");
        assert_eq!(env_get("W3COS_TEST_VAR"), Some("hello".to_string()));

        let all = env_all();
        assert!(all.contains_key("W3COS_TEST_VAR"));
    }

    #[test]
    fn stat_file() {
        let tmp = std::env::temp_dir().join("w3cos_stat_test.txt");
        let path = tmp.to_string_lossy().to_string();
        write_text_file(&path, "12345");

        let info = stat(&path).unwrap();
        assert_eq!(info.size, 5);
        assert!(!info.is_dir);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn nonexistent_file() {
        let rd = read_text_file("/nonexistent/path/file.txt");
        assert!(!rd.ok);
        assert!(!rd.error.is_empty());
    }
}
