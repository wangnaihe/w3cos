//! Load one system runtime, then a bundle, a C plugin, or a web page.
//!
//! ```text
//! w3cos-runtime-host abi
//! w3cos-runtime-host app <app.w3app>
//! w3cos-runtime-host plugin <libapp.so>
//! w3cos-runtime-host page [--headless] <file.html>
//! ```

use std::ffi::{CStr, OsString};
use std::os::raw::c_char;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use w3cos_runtime_host::{
    ensure_app_compatible, find_runtime_library, library_dir_of, prepend_ld_library_path,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("w3cos-runtime-host: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "abi".to_string());
    match command.as_str() {
        "abi" => {
            let runtime = find_runtime_library()?;
            reexec_with_library_path(&runtime)?;
            let lib = open_library(&runtime)?;
            println!("{:#x}", runtime_abi(&lib)?);
        }
        "plugin" => {
            let plugin = args
                .next()
                .context("usage: w3cos-runtime-host plugin <libapp.so>")?;
            let runtime = find_runtime_library()?;
            reexec_with_library_path(&runtime)?;
            let runtime_lib = open_library(&runtime)?;
            let code = run_plugin(&runtime_lib, &plugin)?;
            std::process::exit(code);
        }
        "app" => {
            let app = args
                .next()
                .context("usage: w3cos-runtime-host app <app.w3app>")?;
            let runtime = find_runtime_library()?;
            reexec_with_library_path(&runtime)?;
            let runtime_lib = open_library(&runtime)?;
            let code = run_bundle(&runtime_lib, &app)?;
            std::process::exit(code);
        }
        "page" => {
            let mut headless = false;
            let mut path = None;
            for arg in args {
                if arg == "--headless" {
                    headless = true;
                } else {
                    path = Some(arg);
                }
            }
            let path = path.context("usage: w3cos-runtime-host page [--headless] <file.html>")?;
            let html = std::fs::read_to_string(&path)
                .with_context(|| format!("could not read {path}"))?;
            let runtime = find_runtime_library()?;
            reexec_with_library_path(&runtime)?;
            let lib = open_library(&runtime)?;
            if headless {
                let text = page_text(&lib, &html)?;
                println!("{text}");
            } else {
                let code = run_page(&lib, &path)?;
                std::process::exit(code);
            }
        }
        other => {
            return Err(anyhow!(
                "unknown command {other}. expected abi, app, plugin, or page"
            ));
        }
    }
    Ok(())
}

fn reexec_with_library_path(runtime: &Path) -> Result<()> {
    let dir = library_dir_of(runtime);
    if !prepend_ld_library_path(&dir) {
        return Ok(());
    }
    let exe = std::env::current_exe().context("current exe")?;
    let status = Command::new(exe)
        .args(std::env::args_os().skip(1).collect::<Vec<OsString>>())
        .status()
        .context("re-exec with LD_LIBRARY_PATH")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn open_library(path: &Path) -> Result<libloading::Library> {
    unsafe { libloading::Library::new(path) }
        .with_context(|| format!("dlopen {}", path.display()))
}

fn runtime_abi(lib: &libloading::Library) -> Result<u32> {
    let symbol: libloading::Symbol<unsafe extern "C" fn() -> u32> =
        unsafe { lib.get(b"w3cos_runtime_abi_version\0") }
            .context("runtime is missing w3cos_runtime_abi_version")?;
    Ok(unsafe { symbol() })
}

fn run_plugin(runtime: &libloading::Library, path: &str) -> Result<i32> {
    let host_abi = runtime_abi(runtime)?;
    let plugin = open_library(Path::new(path))?;
    let required: libloading::Symbol<unsafe extern "C" fn() -> u32> =
        unsafe { plugin.get(b"w3cos_plugin_abi_required\0") }
            .context("plugin is missing w3cos_plugin_abi_required")?;
    let app_abi = unsafe { required() };
    ensure_app_compatible(host_abi, app_abi)?;
    let start: libloading::Symbol<unsafe extern "C" fn() -> i32> =
        unsafe { plugin.get(b"w3cos_plugin_main\0") }.context("plugin is missing w3cos_plugin_main")?;
    let code = unsafe { start() };
    if code != 0 {
        return Err(anyhow!(
            "plugin exited with {code}: {}",
            last_error(runtime)
        ));
    }
    Ok(code)
}

fn run_bundle(lib: &libloading::Library, path: &str) -> Result<i32> {
    let symbol: libloading::Symbol<unsafe extern "C" fn(*const c_char) -> i32> =
        unsafe { lib.get(b"w3cos_runtime_run_bundle\0") }
            .context("runtime is missing w3cos_runtime_run_bundle")?;
    let path = std::ffi::CString::new(path).context("bundle path contains an interior NUL")?;
    let code = unsafe { symbol(path.as_ptr()) };
    if code != 0 {
        return Err(anyhow!("bundle exited with {code}: {}", last_error(lib)));
    }
    Ok(code)
}

fn page_text(lib: &libloading::Library, html: &str) -> Result<String> {
    let symbol: libloading::Symbol<unsafe extern "C" fn(*const c_char) -> *const c_char> =
        unsafe { lib.get(b"w3cos_runtime_page_text\0") }
            .context("runtime is missing w3cos_runtime_page_text")?;
    let html = std::ffi::CString::new(html).context("HTML contains an interior NUL")?;
    let ptr = unsafe { symbol(html.as_ptr()) };
    if ptr.is_null() {
        return Err(anyhow!(last_error(lib)));
    }
    Ok(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

fn run_page(lib: &libloading::Library, path: &str) -> Result<i32> {
    let symbol: libloading::Symbol<unsafe extern "C" fn(*const c_char) -> i32> =
        unsafe { lib.get(b"w3cos_runtime_run_page\0") }
            .context("runtime is missing w3cos_runtime_run_page")?;
    let path = std::ffi::CString::new(path).context("page path contains an interior NUL")?;
    let code = unsafe { symbol(path.as_ptr()) };
    if code != 0 {
        return Err(anyhow!("page window exited with {code}: {}", last_error(lib)));
    }
    Ok(code)
}

fn last_error(lib: &libloading::Library) -> String {
    let Ok(symbol) = (unsafe {
        lib.get::<unsafe extern "C" fn() -> *const c_char>(b"w3cos_runtime_last_error\0")
    }) else {
        return "runtime returned an error without w3cos_runtime_last_error".to_string();
    };
    let ptr = unsafe { symbol() };
    if ptr.is_null() {
        return "unknown runtime error".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}
