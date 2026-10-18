//! Source text of module-graph files.
//!
//! Most modules are files on disk. CommonJS files are compiled through
//! synthesized ESM modules (see [`crate::cjs`]): an ESM facade served under the
//! file's own path and a module record served under a virtual sibling path.
//! Every compile stage that needs a module's text reads it here so real files
//! and synthesized modules share one identity.
//!
//! The table is process-wide: synthesized text is a pure function of the
//! file's contents and its resolved dependencies, so concurrent builds that
//! touch the same path write the same text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Debug, Clone)]
struct Synthesized {
    source: Arc<str>,
    /// Static import specifiers of the synthesized text, in order. Graph
    /// discovery uses these instead of the line-based scanner, which must
    /// not look inside a wrapped CommonJS body.
    imports: Arc<[String]>,
}

static SYNTHESIZED: LazyLock<Mutex<HashMap<PathBuf, Synthesized>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read a graph module's source (synthesized text first, then the file).
pub fn read(path: &Path) -> std::io::Result<String> {
    if let Some(entry) = SYNTHESIZED.lock().unwrap().get(path) {
        return Ok(entry.source.to_string());
    }
    std::fs::read_to_string(path)
}

/// Register synthesized source for `path` (replacing any previous text).
pub fn set_synthesized(path: PathBuf, source: String, imports: Vec<String>) {
    SYNTHESIZED.lock().unwrap().insert(
        path,
        Synthesized {
            source: source.into(),
            imports: imports.into(),
        },
    );
}

pub fn is_synthesized(path: &Path) -> bool {
    SYNTHESIZED.lock().unwrap().contains_key(path)
}

/// Static imports of a synthesized module, `None` for plain files.
pub fn synthesized_imports(path: &Path) -> Option<Vec<String>> {
    SYNTHESIZED
        .lock()
        .unwrap()
        .get(path)
        .map(|entry| entry.imports.to_vec())
}

/// Drop synthesized text for `path` (the file is ESM again).
pub fn clear(path: &Path) {
    SYNTHESIZED.lock().unwrap().remove(path);
}
