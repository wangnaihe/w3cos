//! Resolve the system runtime library.
//!
//! The host process does not link `w3cos-runtime`. It dlopens one
//! `libw3cos_runtime.so`, then asks that library to load a bundle or a page.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use w3cos_abi::abi_compatible;

pub fn runtime_library_name() -> &'static str {
    "libw3cos_runtime.so"
}

/// Directories searched for `libw3cos_runtime.so`, most specific first.
pub fn runtime_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("W3COS_RUNTIME_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.join("../lib/w3cos"));
        dirs.push(parent.join("lib/w3cos"));
    }
    dirs.push(PathBuf::from("/usr/lib/w3cos"));
    dirs.push(PathBuf::from("target/shared"));
    dirs
}

pub fn find_runtime_library() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("W3COS_RUNTIME_LIB") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "W3COS_RUNTIME_LIB does not exist: {}",
            path.display()
        ));
    }
    for dir in runtime_search_dirs() {
        let candidate = dir.join(runtime_library_name());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "could not find {name}. Build it with scripts/build-shared-runtime.sh or set W3COS_RUNTIME_LIB.",
        name = runtime_library_name()
    ))
}

pub fn library_dir_of(runtime: &Path) -> PathBuf {
    runtime
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Prepend `dir` to `LD_LIBRARY_PATH` when it is missing.
///
/// glibc reads the variable at process start, so a change made after startup
/// does not affect the next `dlopen`. Callers re-exec when this returns true.
pub fn prepend_ld_library_path(dir: &Path) -> bool {
    let dir = dir.to_string_lossy().to_string();
    if dir.is_empty() {
        return false;
    }
    let current = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    if current.split(':').any(|entry| entry == dir) {
        return false;
    }
    let joined = if current.is_empty() {
        dir
    } else {
        format!("{dir}:{current}")
    };
    unsafe { std::env::set_var("LD_LIBRARY_PATH", joined) };
    true
}

pub fn ensure_app_compatible(host_abi: u32, app_abi: u32) -> Result<()> {
    if abi_compatible(host_abi, app_abi) {
        return Ok(());
    }
    Err(anyhow!(
        "ABI mismatch: runtime {:#x} cannot load app {:#x} (major must match and runtime minor must be >= app minor)",
        host_abi, app_abi
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_abi::{W3COS_RUNTIME_ABI, pack_abi};

    #[test]
    fn search_dirs_include_the_image_path() {
        let dirs = runtime_search_dirs();
        assert!(dirs.iter().any(|dir| dir == Path::new("/usr/lib/w3cos")));
    }

    #[test]
    fn missing_explicit_runtime_is_an_error() {
        let previous = std::env::var_os("W3COS_RUNTIME_LIB");
        unsafe { std::env::set_var("W3COS_RUNTIME_LIB", "/no/such/libw3cos_runtime.so") };
        let error = find_runtime_library().unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        match previous {
            Some(value) => unsafe { std::env::set_var("W3COS_RUNTIME_LIB", value) },
            None => unsafe { std::env::remove_var("W3COS_RUNTIME_LIB") },
        }
    }

    #[test]
    fn compatibility_uses_the_shared_abi_crate() {
        ensure_app_compatible(W3COS_RUNTIME_ABI, W3COS_RUNTIME_ABI).unwrap();
        let error = ensure_app_compatible(W3COS_RUNTIME_ABI, pack_abi(9, 0)).unwrap_err();
        assert!(error.to_string().contains("ABI mismatch"));
    }
}
