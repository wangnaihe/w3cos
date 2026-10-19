//! `electron` / `@w3cos/electron` end to end: AOT-compile an Electron app
//! (main process, preload and page scripts), run it headless and compare
//! with real Electron.
//!
//! References were recorded with Electron 44.9.0 under xvfb
//! (`electron --no-sandbox examples/electron-ipc`; the quick-start through
//! a driver that logs the same `[electron] ...` trace the glue prints with
//! `W3COS_ELECTRON_TRACE=1` and closes the window after load).

use crate::cjs_e2e_tests::{compile_and_run_with_env, write_fixture};

macro_rules! example {
    ($dir:literal, [$($file:literal),*]) => {
        &[$(($file, include_str!(concat!("../../../examples/", $dir, "/", $file)))),*]
    };
}

const QUICK_START: &[(&str, &str)] = example!(
    "electron-quick-start",
    [
        "package.json",
        "main.js",
        "preload.js",
        "renderer.js",
        "index.html",
        "styles.css"
    ]
);

/// Electron 44.9.0 (Node 24.22.0, Chromium 152.0.7977.167).
const QUICK_START_ELECTRON: &str = r#"[electron] window-created 800x600
[electron] page-loaded "Hello World!" "Hello World! We are using Node.js 24.22.0, Chromium 152.0.7977.167, and Electron 44.9.0."
[electron] window-all-closed
[electron] will-quit
"#;

/// Versions differ by design (w3cos-node's Node level, no Chromium).
fn normalize_versions(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("Chromium ") {
        out.push_str(&rest[..at + "Chromium ".len()]);
        rest = &rest[at + "Chromium ".len()..];
        let end = rest.find(',').unwrap_or(rest.len());
        out.push_str("<v>");
        rest = &rest[end..];
    }
    out.push_str(rest);
    let mut normalized = String::new();
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut version = c.to_string();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_digit() || next == '.' {
                    version.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            let dot = version.ends_with('.');
            if dot {
                version.pop();
            }
            if version.matches('.').count() >= 2 {
                normalized.push_str("<v>");
            } else {
                normalized.push_str(&version);
            }
            if dot {
                normalized.push('.');
            }
            continue;
            normalized.push_str(&version);
        } else {
            normalized.push(c);
        }
    }
    normalized
}

#[test]
fn electron_quick_start_matches_electron() {
    let root = write_fixture("w3cos_electron_quick_start", QUICK_START);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_quick_start",
        &[
            ("W3COS_ELECTRON_TRACE", "1"),
            ("W3COS_ELECTRON_EXIT_AFTER_LOAD", "1"),
        ],
    );
    let got: Vec<String> = lines.iter().map(|line| normalize_versions(line)).collect();
    let expected: Vec<String> = QUICK_START_ELECTRON
        .lines()
        .map(normalize_versions)
        .collect();
    assert_eq!(got, expected, "raw output: {lines:#?}");
    // The page really carries the versions the preload wrote.
    assert!(
        lines[1].contains("Node.js 20.0.0,") && lines[1].contains("Electron 44.9.0."),
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

const IPC: &[(&str, &str)] = example!(
    "electron-ipc",
    [
        "package.json",
        "main.js",
        "preload.js",
        "renderer.js",
        "index.html"
    ]
);

/// Electron 44.9.0, byte for byte.
const IPC_ELECTRON: &str = r#"ready true w3cos-electron-ipc 0.1.0
browser-window-created 640x480 false
windows 1 true
did-finish-load IPC round trip
report {"step":"ping","pong":"pong:1","from":"main","title":"IPC round trip","h1":"Hello from the renderer"}
report {"step":"bridge","keys":"done,onAck,onGreeting,ping,platform,report","platform":"string","process":"undefined","require":"undefined"}
done ack:ping,ack:bridge true
closed 0
window-all-closed
will-quit
"#;

#[test]
fn electron_ipc_round_trip_matches_electron() {
    let root = write_fixture("w3cos_electron_ipc", IPC);
    let lines = compile_and_run_with_env(&root, &root.join("main.js"), "w3cos_electron_ipc", &[]);
    let expected: Vec<&str> = IPC_ELECTRON.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const UTILITY: &[(&str, &str)] =
    example!("electron-utility", ["package.json", "main.js", "child.js"]);

/// Electron 44.9.0 (`electron --no-sandbox examples/electron-utility`).
const UTILITY_ELECTRON: &str = r#"channel {"hello":"port","list":[1,2]} 0
forked undefined null null
spawn number true
message {"step":"ready","args":["--mode","sum"],"type":"utility"}
message {"step":"sum","value":5,"ports":0}
kill true
exit number undefined
will-quit
"#;

#[test]
fn electron_utility_process_and_message_channel_match_electron() {
    let root = write_fixture("w3cos_electron_utility", UTILITY);
    let lines =
        compile_and_run_with_env(&root, &root.join("main.js"), "w3cos_electron_utility", &[]);
    let expected: Vec<&str> = UTILITY_ELECTRON.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn version_normalization() {
    assert_eq!(
        normalize_versions(
            "Node.js 24.22.0, Chromium 152.0.7977.167, and Electron 44.9.0. 800x600"
        ),
        "Node.js <v>, Chromium <v>, and Electron <v>. 800x600"
    );
    assert_eq!(
        normalize_versions("Chromium undefined, x"),
        "Chromium <v>, x"
    );
}
