//! Map the one system runtime and enter the desktop that lives inside it.
//!
//! This binary does not link `w3cos-runtime` as an rlib. Layout, rendering,
//! the VM, and the desktop UI are in `libw3cos_runtime.so`.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::process::Command;

use anyhow::{Context, Result};
use w3cos_runtime_host::{find_runtime_library, library_dir_of, prepend_ld_library_path};

fn main() {
    if let Err(error) = run() {
        eprintln!("w3cos-shell: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let runtime = find_runtime_library()?;
    if prepend_ld_library_path(&library_dir_of(&runtime)) {
        let status = Command::new(std::env::current_exe().context("current exe")?)
            .args(std::env::args_os().skip(1))
            .status()
            .context("re-exec with the runtime library path")?;
        std::process::exit(status.code().unwrap_or(1));
    }
    let lib = unsafe { libloading::Library::new(&runtime) }
        .with_context(|| format!("dlopen {}", runtime.display()))?;
    let symbol: libloading::Symbol<unsafe extern "C" fn() -> i32> =
        unsafe { lib.get(b"w3cos_shell_main\0") }.context(
            "libw3cos_runtime.so has no w3cos_shell_main; rebuild it with the desktop feature",
        )?;
    let code = unsafe { symbol() };
    if code != 0 {
        let detail = last_error(&lib);
        anyhow::bail!("desktop exited with {code}: {detail}");
    }
    Ok(())
}

fn last_error(lib: &libloading::Library) -> String {
    let Ok(symbol) = (unsafe {
        lib.get::<unsafe extern "C" fn() -> *const c_char>(b"w3cos_runtime_last_error\0")
    }) else {
        return "no last-error symbol".to_string();
    };
    let ptr = unsafe { symbol() };
    if ptr.is_null() {
        return "unknown error".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}
