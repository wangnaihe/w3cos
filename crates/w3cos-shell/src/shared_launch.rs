//! Hand component bundles, C plugins, and local web pages to the system runtime host.
//!
//! The product shell dlopens the one `libw3cos_runtime.so`. Shared apps, C
//! plugins, and pages still run in a `w3cos-runtime-host` child. That child's
//! page worker does not see the shell's AI-bridge token, and its
//! `capability_gate` starts empty.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// `--shared-app` / `--shared-page` / `--shared-plugin` replace the desktop and
/// wait for the host. Returns false when those flags are absent.
pub fn launch_from_args() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut app = None;
    let mut page = None;
    let mut plugin = None;
    let mut headless = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--shared-app" => {
                app = args.get(index + 1).cloned();
                index += 2;
            }
            "--shared-page" => {
                page = args.get(index + 1).cloned();
                index += 2;
            }
            "--shared-plugin" => {
                plugin = args.get(index + 1).cloned();
                index += 2;
            }
            "--shared-headless" => {
                headless = true;
                index += 1;
            }
            _ => index += 1,
        }
    }
    if let Some(path) = app {
        let code = run_host(&["app", &path], true);
        std::process::exit(code);
    }
    if let Some(path) = plugin {
        let code = run_host(&["plugin", &path], true);
        std::process::exit(code);
    }
    if let Some(path) = page {
        let code = if headless {
            run_host(&["page", "--headless", &path], true)
        } else {
            run_host(&["page", &path], true)
        };
        std::process::exit(code);
    }
    false
}

pub fn configured_path(env_key: &str, fallback: &str) -> String {
    std::env::var(env_key).unwrap_or_else(|_| fallback.to_string())
}

pub fn spawn_detached(kind: &str, path: &str) {
    let code = match kind {
        "app" => run_host(&["app", path], false),
        "page" => run_host(&["page", path], false),
        "plugin" => run_host(&["plugin", path], false),
        _ => 1,
    };
    if code != 0 {
        eprintln!("[W3C OS] shared runtime host exited with {code}");
    }
}

fn run_host(args: &[&str], wait: bool) -> i32 {
    let host = host_binary();
    let mut command = Command::new(&host);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if !wait {
        return match command.spawn() {
            Ok(_) => 0,
            Err(error) => {
                eprintln!(
                    "[W3C OS] could not start {}: {error}",
                    host.display()
                );
                1
            }
        };
    }
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "[W3C OS] could not start {}: {error}",
                host.display()
            );
            1
        }
    }
}

fn host_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("W3COS_RUNTIME_HOST") {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("w3cos-runtime-host");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("/usr/bin/w3cos-runtime-host")
}
