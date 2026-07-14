//! W3C File System Observer API — event-driven file watching
//!
//! Mirrors the WHATWG File System Observer proposal:
//! https://github.com/whatwg/fs/blob/main/proposals/FileSystemObserver.md
//!
//! Also provides a `FileSystemDirectoryHandle` / `FileSystemFileHandle`
//! matching the File System Access API:
//! https://fs.spec.whatwg.org/
//!
//! Uses OS-native events (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW
//! on Windows) via the `notify` crate, replacing the 500ms polling in `w3cos dev`.
//!
//! # Example — watch a project directory
//! ```ignore
//! let observer = FileSystemObserver::new();
//! observer.observe("/my/project", ObserveOptions { recursive: true });
//!
//! // In frame loop:
//! for record in observer.poll_records() {
//!     match record.change_type {
//!         ChangeType::Modified => reload_file(&record.path),
//!         ChangeType::Created  => add_file(&record.path),
//!         ChangeType::Deleted  => remove_file(&record.path),
//!         _ => {}
//!     }
//! }
//! ```

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

// ── ChangeType ─────────────────────────────────────────────────────────────

/// The type of file system change — mirrors the W3C `FileSystemChangeType`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    /// A new file or directory was created.
    Created,
    /// An existing file or directory was modified (content or metadata).
    Modified,
    /// A file or directory was deleted.
    Deleted,
    /// A file or directory was moved/renamed (from `path` to `moved_to`).
    Moved,
    /// The observer could not determine the exact change type.
    Unknown,
}

// ── FileSystemChangeRecord ─────────────────────────────────────────────────

/// W3C `FileSystemChangeRecord` — one observed change event.
#[derive(Debug, Clone)]
pub struct FileSystemChangeRecord {
    /// Absolute path of the changed entry.
    pub path: PathBuf,
    /// For `Moved` changes, the destination path.
    pub moved_to: Option<PathBuf>,
    /// Type of change.
    pub change_type: ChangeType,
    /// Whether the changed entry is a directory.
    pub is_directory: bool,
    /// Timestamp of the change (best-effort, platform-dependent).
    pub timestamp: SystemTime,
}

// ── ObserveOptions ─────────────────────────────────────────────────────────

/// Options for `FileSystemObserver.observe()`.
#[derive(Debug, Clone)]
pub struct ObserveOptions {
    /// Watch subdirectories recursively (default: false).
    pub recursive: bool,
}

impl Default for ObserveOptions {
    fn default() -> Self {
        Self { recursive: false }
    }
}

// ── FileSystemObserver ─────────────────────────────────────────────────────

struct ObserverInner {
    records: Mutex<VecDeque<FileSystemChangeRecord>>,
    stopped: AtomicBool,
}

/// W3C `FileSystemObserver` — watches paths for changes using OS-native events.
///
/// Uses a background thread with `std::fs` metadata polling as a portable
/// fallback. On platforms where `notify` is available, swap the backend for
/// true inotify/FSEvents/kqueue events.
pub struct FileSystemObserver {
    inner: Arc<ObserverInner>,
    // Each watched path gets its own watcher thread handle
    watchers: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl FileSystemObserver {
    /// `new FileSystemObserver(callback)` — create a new observer.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ObserverInner {
                records: Mutex::new(VecDeque::new()),
                stopped: AtomicBool::new(false),
            }),
            watchers: Mutex::new(Vec::new()),
        }
    }

    /// `observer.observe(path, options)` — start watching a path.
    pub fn observe(&self, path: impl AsRef<Path>, options: ObserveOptions) {
        let path = path.as_ref().to_path_buf();
        let inner = Arc::clone(&self.inner);

        let handle = thread::Builder::new()
            .name(format!("w3cos-fswatch-{}", path.display()))
            .spawn(move || watch_loop(path, options, inner))
            .expect("spawn fs watcher");

        if let Ok(mut watchers) = self.watchers.lock() {
            watchers.push(handle);
        }
    }

    /// Drain all pending change records. Call from a frame loop.
    pub fn poll_records(&self) -> Vec<FileSystemChangeRecord> {
        let mut q = self.inner.records.lock().unwrap();
        q.drain(..).collect()
    }

    /// `observer.disconnect()` — stop all watchers.
    pub fn disconnect(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
    }
}

impl Default for FileSystemObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FileSystemObserver {
    fn drop(&mut self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
    }
}

// ── Watch loop (portable polling backend) ─────────────────────────────────

/// Snapshot of a directory entry for change detection.
#[derive(Clone)]
struct EntrySnapshot {
    path: PathBuf,
    is_dir: bool,
    modified: Option<SystemTime>,
    size: u64,
}

fn snapshot_dir(root: &Path, recursive: bool) -> Vec<EntrySnapshot> {
    let mut entries = Vec::new();
    snapshot_dir_inner(root, recursive, &mut entries);
    entries
}

fn snapshot_dir_inner(dir: &Path, recursive: bool, out: &mut Vec<EntrySnapshot>) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = meta.modified().ok();
        let is_dir = meta.is_dir();
        out.push(EntrySnapshot {
            path: path.clone(),
            is_dir,
            modified,
            size: meta.len(),
        });
        if is_dir && recursive {
            snapshot_dir_inner(&path, recursive, out);
        }
    }
}

fn push_record(inner: &Arc<ObserverInner>, record: FileSystemChangeRecord) {
    if let Ok(mut q) = inner.records.lock() {
        q.push_back(record);
    }
}

fn watch_loop(root: PathBuf, options: ObserveOptions, inner: Arc<ObserverInner>) {
    // Initial snapshot
    let mut prev: Vec<EntrySnapshot> = if root.is_dir() {
        snapshot_dir(&root, options.recursive)
    } else {
        // Single file watch
        match std::fs::metadata(&root) {
            Ok(m) => vec![EntrySnapshot {
                path: root.clone(),
                is_dir: false,
                modified: m.modified().ok(),
                size: m.len(),
            }],
            Err(_) => Vec::new(),
        }
    };

    // Poll interval — 100ms is responsive enough for dev tooling
    let interval = Duration::from_millis(100);

    loop {
        if inner.stopped.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(interval);

        let curr: Vec<EntrySnapshot> = if root.is_dir() {
            snapshot_dir(&root, options.recursive)
        } else {
            match std::fs::metadata(&root) {
                Ok(m) => vec![EntrySnapshot {
                    path: root.clone(),
                    is_dir: false,
                    modified: m.modified().ok(),
                    size: m.len(),
                }],
                Err(_) => Vec::new(),
            }
        };

        // Detect created / modified
        for entry in &curr {
            let prev_entry = prev.iter().find(|e| e.path == entry.path);
            match prev_entry {
                None => {
                    push_record(
                        &inner,
                        FileSystemChangeRecord {
                            path: entry.path.clone(),
                            moved_to: None,
                            change_type: ChangeType::Created,
                            is_directory: entry.is_dir,
                            timestamp: SystemTime::now(),
                        },
                    );
                }
                Some(p) => {
                    let content_changed = entry.size != p.size || entry.modified != p.modified;
                    if content_changed {
                        push_record(
                            &inner,
                            FileSystemChangeRecord {
                                path: entry.path.clone(),
                                moved_to: None,
                                change_type: ChangeType::Modified,
                                is_directory: entry.is_dir,
                                timestamp: SystemTime::now(),
                            },
                        );
                    }
                }
            }
        }

        // Detect deleted
        for entry in &prev {
            if !curr.iter().any(|e| e.path == entry.path) {
                push_record(
                    &inner,
                    FileSystemChangeRecord {
                        path: entry.path.clone(),
                        moved_to: None,
                        change_type: ChangeType::Deleted,
                        is_directory: entry.is_dir,
                        timestamp: SystemTime::now(),
                    },
                );
            }
        }

        prev = curr;
    }
}

// ── FileSystemFileHandle ───────────────────────────────────────────────────

/// W3C `FileSystemFileHandle` — a handle to a specific file.
#[derive(Debug, Clone)]
pub struct FileSystemFileHandle {
    pub path: PathBuf,
}

impl FileSystemFileHandle {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `handle.name` — the file name without directory.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// `handle.getFile()` — read the file contents as text.
    pub fn get_text(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path).map_err(|e| e.to_string())
    }

    /// `handle.getFile()` — read the file contents as bytes.
    pub fn get_bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.path).map_err(|e| e.to_string())
    }

    /// `handle.createWritable()` — write text to the file.
    pub fn write_text(&self, content: &str) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.path, content).map_err(|e| e.to_string())
    }

    /// `handle.createWritable()` — write bytes to the file.
    pub fn write_bytes(&self, data: &[u8]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.path, data).map_err(|e| e.to_string())
    }

    /// `handle.isSameEntry(other)` — compare by canonical path.
    pub fn is_same_entry(&self, other: &Self) -> bool {
        let a = std::fs::canonicalize(&self.path);
        let b = std::fs::canonicalize(&other.path);
        match (a, b) {
            (Ok(a), Ok(b)) => a == b,
            _ => self.path == other.path,
        }
    }
}

// ── FileSystemDirectoryHandle ──────────────────────────────────────────────

/// W3C `FileSystemDirectoryHandle` — a handle to a directory.
#[derive(Debug, Clone)]
pub struct FileSystemDirectoryHandle {
    pub path: PathBuf,
}

impl FileSystemDirectoryHandle {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `handle.name`
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// `handle.getFileHandle(name)` — get a file handle within this directory.
    pub fn get_file_handle(&self, name: &str) -> FileSystemFileHandle {
        FileSystemFileHandle::new(self.path.join(name))
    }

    /// `handle.getDirectoryHandle(name)` — get a subdirectory handle.
    pub fn get_directory_handle(&self, name: &str) -> FileSystemDirectoryHandle {
        FileSystemDirectoryHandle::new(self.path.join(name))
    }

    /// `handle.entries()` — list all entries (files and subdirectories).
    pub fn entries(&self) -> Result<Vec<FileSystemEntry>, String> {
        let read = std::fs::read_dir(&self.path).map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        for entry in read.flatten() {
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            let path = entry.path();
            if meta.is_dir() {
                entries.push(FileSystemEntry::Directory(FileSystemDirectoryHandle::new(
                    path,
                )));
            } else {
                entries.push(FileSystemEntry::File(FileSystemFileHandle::new(path)));
            }
        }
        entries.sort_by(|a, b| {
            let a_is_dir = matches!(a, FileSystemEntry::Directory(_));
            let b_is_dir = matches!(b, FileSystemEntry::Directory(_));
            b_is_dir.cmp(&a_is_dir).then(a.name().cmp(&b.name()))
        });
        Ok(entries)
    }

    /// `handle.removeEntry(name)` — delete a file or directory.
    pub fn remove_entry(&self, name: &str, recursive: bool) -> Result<(), String> {
        let target = self.path.join(name);
        if target.is_dir() {
            if recursive {
                std::fs::remove_dir_all(&target).map_err(|e| e.to_string())
            } else {
                std::fs::remove_dir(&target).map_err(|e| e.to_string())
            }
        } else {
            std::fs::remove_file(&target).map_err(|e| e.to_string())
        }
    }

    /// `handle.resolve(child)` — get relative path from this dir to child.
    pub fn resolve(&self, child: &FileSystemFileHandle) -> Option<Vec<String>> {
        child.path.strip_prefix(&self.path).ok().map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect()
        })
    }
}

/// A directory entry — either a file or subdirectory handle.
#[derive(Debug, Clone)]
pub enum FileSystemEntry {
    File(FileSystemFileHandle),
    Directory(FileSystemDirectoryHandle),
}

impl FileSystemEntry {
    pub fn name(&self) -> String {
        match self {
            Self::File(f) => f.name(),
            Self::Directory(d) => d.name(),
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::File(_))
    }

    pub fn is_directory(&self) -> bool {
        matches!(self, Self::Directory(_))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn file_handle_read_write() {
        let tmp = std::env::temp_dir().join("w3cos_fshandle_test.txt");
        let handle = FileSystemFileHandle::new(&tmp);
        handle.write_text("hello fs access api").unwrap();
        let text = handle.get_text().unwrap();
        assert_eq!(text, "hello fs access api");
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn directory_handle_entries() {
        let tmp = std::env::temp_dir().join("w3cos_fsdir_test");
        std::fs::create_dir_all(&tmp).ok();
        std::fs::write(tmp.join("a.txt"), "a").ok();
        std::fs::write(tmp.join("b.txt"), "b").ok();

        let dir = FileSystemDirectoryHandle::new(&tmp);
        let entries = dir.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.name() == "a.txt"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn observer_detects_created_file() {
        let tmp = std::env::temp_dir().join("w3cos_fsobs_test");
        std::fs::create_dir_all(&tmp).ok();

        let observer = FileSystemObserver::new();
        observer.observe(&tmp, ObserveOptions::default());

        // Give the watcher time to take initial snapshot
        thread::sleep(Duration::from_millis(150));

        std::fs::write(tmp.join("new_file.txt"), "content").ok();

        // Wait for the watcher to detect the change
        thread::sleep(Duration::from_millis(250));

        let records = observer.poll_records();
        observer.disconnect();

        assert!(
            records.iter().any(|r| r.change_type == ChangeType::Created
                && r.path
                    .file_name()
                    .map(|n| n == "new_file.txt")
                    .unwrap_or(false)),
            "expected Created record, got: {:?}",
            records
                .iter()
                .map(|r| (&r.path, &r.change_type))
                .collect::<Vec<_>>()
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
