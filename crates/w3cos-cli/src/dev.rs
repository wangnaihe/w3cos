//! Dev pipeline: multi-file watch, Chrome DevTools, web static server.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

pub struct DevConfig {
    pub devtools: bool,
    pub devtools_port: u16,
    pub web_port: u16,
}

impl Default for DevConfig {
    fn default() -> Self {
        Self {
            devtools: true,
            devtools_port: 9229,
            web_port: 5173,
        }
    }
}

pub fn watch_paths_for(input: &Path) -> Result<Vec<PathBuf>> {
    w3cos_compiler::collect_watch_paths(input)
}

pub fn snapshot_mtimes(paths: &[PathBuf]) -> HashMap<PathBuf, Option<SystemTime>> {
    paths.iter().map(|p| (p.clone(), file_mtime(p))).collect()
}

pub fn any_mtime_changed(paths: &[PathBuf], last: &HashMap<PathBuf, Option<SystemTime>>) -> bool {
    paths
        .iter()
        .any(|p| file_mtime(p) != *last.get(p).unwrap_or(&None))
}

pub fn refresh_mtimes(paths: &[PathBuf], last: &mut HashMap<PathBuf, Option<SystemTime>>) {
    for p in paths {
        last.insert(p.clone(), file_mtime(p));
    }
}

pub fn wait_for_change(paths: &[PathBuf], last: &mut HashMap<PathBuf, Option<SystemTime>>) {
    println!("👀 Waiting for file changes...");
    loop {
        std::thread::sleep(Duration::from_millis(400));
        if any_mtime_changed(paths, last) {
            refresh_mtimes(paths, last);
            return;
        }
    }
}

pub fn spawn_native_app(bin: &Path, config: &DevConfig) -> Result<Child> {
    let mut cmd = Command::new(bin);
    if config.devtools {
        cmd.env("W3COS_DEVTOOLS_PORT", config.devtools_port.to_string());
        print_devtools_hint(config.devtools_port);
    }
    cmd.spawn().context("Failed to run compiled binary")
}

pub fn print_devtools_hint(port: u16) {
    println!("🔧 Chrome DevTools on 127.0.0.1:{port}");
    println!("   1. Open chrome://inspect");
    println!("   2. Configure → add 127.0.0.1:{port}");
    println!("   3. Inspect the listed target\n");
}

pub fn print_mobile_devtools_hint(port: u16, platform: &str) {
    match platform {
        "android" => {
            println!("🔧 Chrome DevTools (Android) on port {port}");
            println!("   adb forward tcp:{port} tcp:{port}");
            println!("   Chrome → chrome://inspect → Configure → 127.0.0.1:{port}\n");
        }
        "ios" => {
            println!("🔧 Chrome DevTools (iOS Simulator) on port {port}");
            println!("   Chrome → chrome://inspect → Configure → 127.0.0.1:{port}\n");
        }
        _ => print_devtools_hint(port),
    }
}

pub fn setup_android_devtools_forward(port: u16) {
    let ok = Command::new("adb")
        .args(["forward", &format!("tcp:{port}"), &format!("tcp:{port}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        println!("✅ adb forward tcp:{port} tcp:{port}");
    } else {
        println!("ℹ️  Could not run adb forward — connect device/emulator and run:");
        println!("   adb forward tcp:{port} tcp:{port}");
    }
}

pub fn start_web_server(root: &Path, port: u16) -> Result<Arc<AtomicBool>> {
    let root = root.canonicalize().context("web output dir")?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("❌ Web server bind failed on :{port}: {e}");
                return;
            }
        };
        println!("🌐 Web preview: http://127.0.0.1:{port}/index.html");
        while !stop_flag.load(Ordering::Relaxed) {
            listener.set_nonblocking(true).ok();
            match listener.accept() {
                Ok((mut stream, _)) => {
                    serve_static(&root, &mut stream);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => eprintln!("web server accept error: {e}"),
            }
        }
    });
    Ok(stop)
}

fn serve_static(root: &Path, stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return;
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let rel = if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };
    let file_path = root.join(rel);
    let (status, body, content_type) = if file_path.starts_with(root) && file_path.is_file() {
        match std::fs::read(&file_path) {
            Ok(bytes) => ("200 OK", bytes, mime_for(&file_path)),
            Err(_) => ("500 Internal Server Error", Vec::new(), "text/plain"),
        }
    } else {
        ("404 Not Found", b"Not found".to_vec(), "text/plain")
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
