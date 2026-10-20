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

const PLATFORM: &[(&str, &str)] = example!(
    "electron-platform",
    ["package.json", "main.js", "page.html", "data.json"]
);

/// Electron 44.9.0 (`electron --no-sandbox examples/electron-platform` under
/// xvfb), byte for byte: `vscode-file://` through `protocol.handle` +
/// `net.fetch` + `loadURL`, nativeTheme, screen, safeStorage.
const PLATFORM_ELECTRON: &str = r#"screen-before-ready-throws true
theme system boolean false false false false
dark dark true
light light false
bad-source TypeError "Error processing argument at index 0, conversion failure from blue"
after-bad light
updates 1
display-keys accelerometerSupport,bounds,colorDepth,colorSpace,depthPerComponent,detected,displayFrequency,id,internal,label,maximumCursorSize,monochrome,nativeOrigin,rotation,scaleFactor,size,touchSupport,workArea,workAreaSize
display number string number number 0 0 true true 0 false unknown false true
all true true true
nearest true true
cursor number number function function
ss true string
ss-enc true false
ss-dec secret ✓ token
ss-empty ""
ss-dec-type Error "Expected the first argument of decryptString() to be a buffer"
ss-dec-bad Error "Error while decrypting the ciphertext provided to safeStorage.decryptString. Ciphertext does not appear to be encrypted."
ss-enc-type TypeError "Error processing argument at index 0, conversion failure from "
handled-before false
handled-after true
handler vscode-app data.json GET
fetch 200 true {"answer":42}
handler other x GET
denied 403 denied
double-handle Failed to register protocol: vscode-file
handler vscode-app page.html GET
loaded true vscode-file page
handled-unhandled false
"#;

#[test]
fn electron_protocol_theme_screen_safe_storage_match_electron() {
    let root = write_fixture("w3cos_electron_platform", PLATFORM);
    // A throwaway keyring: the test must not create the user's key.
    let keyring = root.join("keyring");
    let keyring = keyring.to_str().unwrap();
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_platform",
        &[("W3COS_KEYRING_DIR", keyring)],
    );
    let expected: Vec<&str> = PLATFORM_ELECTRON.lines().collect();
    assert_eq!(lines, expected);
    assert!(root.join("keyring/safe-storage.key").is_file());
    std::fs::remove_dir_all(root).ok();
}

const DOM_GAPS: &[(&str, &str)] = example!(
    "electron-dom-gaps",
    [
        "package.json",
        "main.js",
        "preload.js",
        "renderer.js",
        "index.html"
    ]
);

/// Electron 44.9.0 (`electron --no-sandbox examples/electron-dom-gaps` under
/// xvfb), byte for byte: the renderer-side CSSOM / DOM surface Code-OSS and
/// Monaco lean on (constructable + adopted sheets, var()/inheritance,
/// `:is()`/`:where()`, inline `!important`, client/scroll geometry, DnD
/// DataTransfer, focus/tabIndex, `queryCommandSupported`).
const DOM_GAPS_ELECTRON: &str = r#"adopted.ctor => [2,"rgb(255, 0, 0)","4px",1,3,"rgb(0, 0, 255)","rgb(255, 0, 0)","rgb(0, 0, 0)"]
adopted.replace => "function"
stylesheets => [1,true,":root"]
style.inject => ["7px",2,"0px"]
computed.var => ["rgb(1, 2, 3)","6px","rgb(1, 2, 3)"]
computed.inherit => ["rgb(9, 8, 7)","20px"]
computed.nth => ["700/list-item","400/none","700/list-item","400/list-item"]
computed.setProperty => ["rgb(4, 5, 6)","rgb(4, 5, 6)","important","rgb(7, 7, 7)","--fg: rgb(4, 5, 6); color: rgb(7, 7, 7) !important;","rgb(1, 2, 3)"]
selectors => [2,2,1,2,1,true,true,2]
innerHTML => "<li data-x=\"abc\">four</li>|<span id=\"child\">child</span>"
rect => [130,50,130,126,50]
scroll.element => [500,50,30,100,20,110,true,0]
scroll.window => ["number",0,"object","function"]
dnd.datatransfer => ["hello","hello","text/plain,application/vnd.code.tree",2,"none","none",0,"application/vnd.code.tree","none","none"]
dnd.events => [true,false,true,"dragstart:true:true:x true dragenter:true:true:x true dragover:true:true:x false drop:true:true:x true dragend:true:true:x true"]
svg => ["http://www.w3.org/2000/svg","svg","svg","path",true,"0 0 16 16","http://www.w3.org/1999/xhtml"]
clipboard.event => ["copy",true,"c","function",true]
globals => ["function","function","function","function","function","function","aGk=","hi",2]
css.ns => [true,true,false,"a\\.b\\#c","\\31 x"]
focus => [true,0,-1,0,"0",true]
dialog => [true,false,"x","function"]
shadow => [true,"s","open"]
misc => [true,"object","function","function"]
idle number boolean
"#;

#[test]
fn electron_renderer_dom_gaps_match_electron() {
    let root = write_fixture("w3cos_electron_dom_gaps", DOM_GAPS);
    let lines =
        compile_and_run_with_env(&root, &root.join("main.js"), "w3cos_electron_dom_gaps", &[]);
    let expected: Vec<&str> = DOM_GAPS_ELECTRON.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

/// A `<script type="module">` page and its imports evaluate when the window
/// loads the page (after the preload, before `DOMContentLoaded` and
/// `did-finish-load`), never when the main process requires `electron`.
const ESM_PAGE: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-esm-page", "version": "0.1.0", "main": "main.js" }"#,
    ),
    (
        "main.js",
        r#"const { app, BrowserWindow, ipcMain } = require('electron')
const path = require('node:path')
console.log('main start', typeof globalThis.pageEvaluated)
let win
ipcMain.on('line', (_event, line) => console.log(line))
app.whenReady().then(() => {
  console.log('ready', typeof globalThis.pageEvaluated)
  win = new BrowserWindow({ show: false, webPreferences: { preload: path.join(__dirname, 'preload.js') } })
  win.webContents.on('did-finish-load', () => { console.log('did-finish-load', typeof globalThis.pageEvaluated); setTimeout(() => win.close(), 0) })
  win.loadFile('index.html')
})
app.on('window-all-closed', () => app.quit())
"#,
    ),
    (
        "preload.js",
        r#"const { ipcRenderer, contextBridge } = require('electron')
contextBridge.exposeInMainWorld('bridge', { line: (text) => ipcRenderer.send('line', text) })
window.addEventListener('DOMContentLoaded', () => window.bridge.line('DOMContentLoaded'))
"#,
    ),
    (
        "page.js",
        r#"import { label } from './lib/label.js'
globalThis.pageEvaluated = true
window.bridge.line('page ' + label() + ' ' + document.querySelector('h1').textContent)
"#,
    ),
    (
        "lib/label.js",
        r#"export function label() { return typeof window.bridge.line + ':ipcRenderer-free' }
"#,
    ),
    (
        "index.html",
        r#"<!DOCTYPE html><html><head><title>ESM page</title></head><body><h1>Hello</h1><script type="module" src="./page.js"></script></body></html>"#,
    ),
];

#[test]
fn electron_module_page_script_evaluates_when_the_page_loads() {
    let root = write_fixture("w3cos_electron_esm_page", ESM_PAGE);
    let lines =
        compile_and_run_with_env(&root, &root.join("main.js"), "w3cos_electron_esm_page", &[]);
    // Startup never evaluates the page; `did-finish-load` fires after the
    // module ran. Renderer→main IPC lines arrive asynchronously, so only
    // their presence is checked.
    assert_eq!(
        lines[..2],
        ["main start undefined", "ready undefined"],
        "{lines:#?}"
    );
    let mut rest: Vec<&str> = lines[2..].iter().map(String::as_str).collect();
    rest.sort_unstable();
    assert_eq!(
        rest,
        [
            "DOMContentLoaded",
            "did-finish-load boolean",
            "page function:ipcRenderer-free Hello",
        ],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}
