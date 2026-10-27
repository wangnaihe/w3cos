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

/// The Code-OSS preload destructures `webFrame` and `webUtils` from
/// `electron` and sets the zoom level from the window configuration; its
/// main process intercepts `file:` with `protocol.handle`.
const WEB_FRAME: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-web-frame", "version": "0.1.0", "main": "main.js" }"#,
    ),
    (
        "main.js",
        r#"const { app, BrowserWindow, ipcMain, protocol, net } = require('electron')
const path = require('node:path')
const { pathToFileURL } = require('node:url')
let win
ipcMain.on('line', (event, line) => {
  console.log(line)
  // Code-OSS validatedIpcMain only accepts main-frame senders.
  const frame = event.senderFrame
  console.log('sender-frame', frame === event.sender.mainFrame, frame.parent, frame.top === frame, frame.url === event.sender.getURL(), frame.framesInSubtree.length)
  setTimeout(() => win.close(), 0)
})
app.whenReady().then(async () => {
  // Code-OSS's protocolMainService intercepts file: and refuses it.
  protocol.handle('file', () => new Response('refused', { status: 403 }))
  try { protocol.handle('file', () => null); console.log('double-handle ok') } catch (e) { console.log('double-handle throws') }
  const href = pathToFileURL(path.join(__dirname, 'index.html')).href
  const handled = await net.fetch(href)
  const bypass = await net.fetch(href, { bypassCustomProtocolHandlers: true })
  console.log('file-handle', handled.status, bypass.status, (await bypass.text()).startsWith('<!DOCTYPE html>'))
  protocol.unhandle('file')
  console.log('file-unhandled', (await net.fetch(href)).status)
  win = new BrowserWindow({ show: false, webPreferences: { preload: path.join(__dirname, 'preload.js') } })
  win.loadFile('index.html')
})
app.on('window-all-closed', () => app.quit())
"#,
    ),
    (
        "preload.js",
        r#"const { ipcRenderer, webFrame, webUtils } = require('electron')
const before = [webFrame.getZoomLevel(), webFrame.getZoomFactor()]
webFrame.setZoomLevel(2)
const level = webFrame.getZoomLevel()
const factor = Math.round(webFrame.getZoomFactor() * 100) / 100
webFrame.setZoomFactor(1)
ipcRenderer.send('line', JSON.stringify({
  before, level, factor, reset: webFrame.getZoomLevel(),
  utils: typeof webUtils.getPathForFile,
  frame: ['setZoomLevel', 'getZoomLevel', 'setZoomFactor', 'getZoomFactor', 'setVisualZoomLevelLimits'].map((k) => typeof webFrame[k]).join(','),
}))
"#,
    ),
    (
        "index.html",
        r#"<!DOCTYPE html><html><head><title>webFrame</title></head><body></body></html>"#,
    ),
];

#[test]
fn electron_web_frame_web_utils_and_file_protocol_handle() {
    let root = write_fixture("w3cos_electron_web_frame", WEB_FRAME);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_web_frame",
        &[],
    );
    assert_eq!(
        lines,
        [
            "double-handle throws",
            "file-handle 403 200 true",
            "file-unhandled 200",
            r#"{"before":[0,1],"level":2,"factor":1.44,"reset":0,"utils":"function","frame":"function,function,function,function,function"}"#,
            "sender-frame true null true true 1",
        ],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Code-OSS main imports these modules at startup; on Linux they only need
/// to exist and answer with Electron's shapes.
const MAIN_SERVICES: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-main-services", "version": "0.1.0", "main": "main.js" }"#,
    ),
    (
        "main.js",
        r#"const { app, BrowserWindow, powerMonitor, powerSaveBlocker, systemPreferences, globalShortcut, clipboard, nativeImage, Notification, contentTracing, desktopCapturer } = require('electron')
app.whenReady().then(async () => {
  let resumed = 0
  powerMonitor.on('resume', () => resumed++)
  powerMonitor.emit('resume')
  console.log('power', resumed, powerMonitor.isOnBatteryPower(), powerMonitor.getSystemIdleState(60), powerMonitor.getSystemIdleTime())
  const id = powerSaveBlocker.start('prevent-app-suspension')
  const started = powerSaveBlocker.isStarted(id)
  powerSaveBlocker.stop(id)
  console.log('blocker', typeof id, started, powerSaveBlocker.isStarted(id))
  console.log('prefs', typeof systemPreferences.on, typeof systemPreferences.getAccentColor())
  const first = globalShortcut.register('CommandOrControl+X', () => {})
  const again = globalShortcut.register('CommandOrControl+X', () => {})
  const registered = globalShortcut.isRegistered('CommandOrControl+X')
  globalShortcut.unregisterAll()
  console.log('shortcut', first, again, registered, globalShortcut.isRegistered('CommandOrControl+X'))
  clipboard.writeText('hello')
  const text = clipboard.readText()
  clipboard.write({ text: 'a', html: '<b>a</b>' })
  console.log('clipboard', text, clipboard.readHTML(), clipboard.availableFormats().join(','), clipboard.readText('selection') === '')
  const image = nativeImage.createFromDataURL('data:image/png;base64,iVBORw0KGgo=')
  console.log('image', nativeImage.createEmpty().isEmpty(), image.isEmpty(), image.toPNG().length, image.toDataURL())
  console.log('notification', Notification.isSupported(), new Notification({ title: 't' }).title)
  console.log('tracing', JSON.stringify(await contentTracing.getCategories()), JSON.stringify(await desktopCapturer.getSources({ types: ['screen'] })))
  const win = new BrowserWindow({ show: false })
  const handle = win.getNativeWindowHandle()
  console.log('window', Buffer.isBuffer(handle), handle.length, handle.buffer instanceof ArrayBuffer, handle.readUInt32LE(0) === win.id, win.getMediaSourceId() === 'window:' + win.id + ':0')
  const sized = new BrowserWindow({ show: false, minWidth: 400, minHeight: 270 })
  const before = sized.getMinimumSize().join('x')
  sized.setMinimumSize(500, 300)
  console.log('minsize', before, sized.getMinimumSize().join('x'), win.getMinimumSize().join('x'), win.getMaximumSize().join('x'), sized.getSize().join('x'))
  app.quit()
})
"#,
    ),
];

#[test]
fn electron_main_process_service_modules() {
    let root = write_fixture("w3cos_electron_main_services", MAIN_SERVICES);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_main_services",
        &[],
    );
    assert_eq!(
        lines,
        [
            "power 1 false active 0",
            "blocker number true false",
            "prefs function string",
            "shortcut true false true false",
            "clipboard hello <b>a</b> text/plain,text/html true",
            "image true false 8 data:image/png;base64,iVBORw0KGgo=",
            "notification false t",
            "tracing [] []",
            "window true 8 true true true",
            "minsize 400x270 500x300 0x0 0x0 800x600",
        ],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Code-OSS's window boot: the main process passes the window-config IPC
/// channel through `webPreferences.additionalArguments`; the (classic)
/// preload reads it from the renderer's `process.argv` and `invoke()`s it;
/// the workbench module script (placed after `</body>`) awaits that
/// configuration in an async IIFE before painting its splash.
const WINDOW_CONFIG: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-window-config", "version": "0.1.0", "main": "main.js" }"#,
    ),
    (
        "main.js",
        r#"const { app, BrowserWindow, ipcMain } = require('electron')
const path = require('node:path')
const argc = process.argv.length
let win
ipcMain.handle('vscode:windowConfig', () => ({ zoomLevel: 0, windowId: 7 }))
ipcMain.on('vscode:line', (_event, line) => { console.log(line); setTimeout(() => win.close(), 0) })
app.whenReady().then(() => {
  win = new BrowserWindow({ show: false, webPreferences: {
    preload: path.join(__dirname, 'preload.js'),
    additionalArguments: ['--vscode-window-config=vscode:windowConfig'],
  } })
  win.webContents.on('did-finish-load', () => {
    console.log('did-finish-load main-argv-unchanged', process.argv.length === argc)
  })
  win.loadFile('workbench.html')
})
app.on('window-all-closed', () => app.quit())
"#,
    ),
    (
        "preload.js",
        r#"(function () {
  const { ipcRenderer, contextBridge } = require('electron')
  function parseArgv(key) {
    for (const arg of process.argv) {
      if (arg.indexOf(`--${key}=`) === 0) return arg.split('=')[1]
    }
    return undefined
  }
  const resolveConfiguration = (async () => {
    const channel = parseArgv('vscode-window-config')
    if (!channel) throw new Error('Preload: did not find expected vscode-window-config in renderer process arguments list.')
    return ipcRenderer.invoke(channel)
  })()
  contextBridge.exposeInMainWorld('vscode', {
    ipcRenderer: { send: (channel, ...args) => ipcRenderer.send(channel, ...args) },
    context: { async resolveConfiguration() { return resolveConfiguration } },
  })
})()
"#,
    ),
    (
        "workbench.js",
        r#"import { splashText } from './splash.js'
(async function () {
  const preloadGlobals = window.vscode
  try {
    const configuration = await preloadGlobals.context.resolveConfiguration()
    const splash = document.createElement('div')
    splash.textContent = splashText(configuration)
    document.body.appendChild(splash)
    preloadGlobals.ipcRenderer.send('vscode:line', 'workbench ' + document.body.textContent.trim())
  } catch (error) {
    preloadGlobals.ipcRenderer.send('vscode:line', 'error ' + error.message)
  }
})()
"#,
    ),
    (
        "splash.js",
        r#"export function splashText(configuration) { return 'splash ' + configuration.windowId }
"#,
    ),
    (
        "workbench.html",
        r#"<!DOCTYPE html>
<html>
	<head><meta charset="utf-8" /></head>
	<body aria-label="">
	</body>
	<!-- Startup (do not modify order of script tags!) -->
	<script src="./workbench.js" type="module"></script>
</html>
"#,
    ),
];

#[test]
fn electron_additional_arguments_reach_the_preload_process_argv() {
    let root = write_fixture("w3cos_electron_window_config", WINDOW_CONFIG);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_window_config",
        &[],
    );
    // Renderer→main IPC lines arrive asynchronously: compare sorted.
    let mut sorted: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| !line.starts_with("[electron]"))
        .collect();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        [
            "did-finish-load main-argv-unchanged true",
            "workbench splash 7"
        ],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn electron_renderer_trace_logs_scripts_and_ipc() {
    let root = write_fixture("w3cos_electron_renderer_trace", WINDOW_CONFIG);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_renderer_trace",
        &[("W3COS_ELECTRON_TRACE", "2")],
    );
    let traced: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains("-start ") || line.contains("-done ") || line.contains("ipc-"))
        .collect();
    let position = |needle: &str| {
        traced
            .iter()
            .position(|line| line.ends_with(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in {lines:#?}"))
    };
    // The preload evaluates first and asks for the window config; the
    // page script runs after it and reports back over ipc.
    let preload_start = position("preload-start preload.js");
    let invoke = position("ipc-invoke vscode:windowConfig");
    let invoke_done = position("ipc-invoke-done vscode:windowConfig");
    let script_start = position("script-start workbench.js");
    let send = position("ipc-send vscode:line");
    assert!(
        preload_start < invoke && invoke < invoke_done,
        "{traced:#?}"
    );
    assert!(
        preload_start < script_start && invoke_done < send,
        "{traced:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Code-OSS's shared-process handshake: main transfers one end of a
/// MessageChannelMain to a utility process and the other to the page
/// (`webContents.postMessage(channel, nonce, [port])`); the preload forwards
/// it with `window.postMessage(nonce, '*', event.ports)`.
const PORT_TRANSFER: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-port-transfer", "version": "0.1.0", "main": "main.js" }"#,
    ),
    (
        "main.js",
        r#"const { app, BrowserWindow, ipcMain, utilityProcess, MessageChannelMain } = require('electron')
const path = require('node:path')
let win
let child
app.whenReady().then(() => {
  child = utilityProcess.fork(path.join(__dirname, 'child.js'))
  ipcMain.on('report', (_event, line) => {
    console.log(line)
    if (line.startsWith('renderer got')) {
      child.kill()
      setTimeout(() => win.close(), 0)
    }
  })
  win = new BrowserWindow({ show: false, webPreferences: { preload: path.join(__dirname, 'preload.js') } })
  win.webContents.on('did-finish-load', () => {
    const { port1, port2 } = new MessageChannelMain()
    child.postMessage({ hello: 'child' }, [port2])
    win.webContents.postMessage('port', 'nonce-1', [port1])
  })
  win.loadFile('index.html')
})
app.on('window-all-closed', () => app.quit())
"#,
    ),
    (
        "child.js",
        r#"process.parentPort.on('message', (event) => {
  const port = event.ports[0]
  port.on('message', (e) => port.postMessage('pong:' + e.data + ':' + event.data.hello))
  port.start()
})
"#,
    ),
    (
        "preload.js",
        r#"const { contextBridge, ipcRenderer } = require('electron')
ipcRenderer.on('port', (event, nonce) => window.postMessage(nonce, '*', event.ports))
contextBridge.exposeInMainWorld('bridge', { report: (line) => ipcRenderer.send('report', line) })
"#,
    ),
    (
        "renderer.js",
        r#"window.addEventListener('message', (event) => {
  if (event.data !== 'nonce-1') return
  const port = event.ports[0]
  window.bridge.report('renderer ports ' + event.ports.length)
  port.onmessage = (e) => window.bridge.report('renderer got ' + e.data)
  port.postMessage('ping')
})
"#,
    ),
    (
        "index.html",
        r#"<!DOCTYPE html><html><head><title>ports</title></head><body><script src="renderer.js"></script></body></html>"#,
    ),
];

#[test]
fn electron_message_ports_transfer_to_the_page_and_a_utility_process() {
    let root = write_fixture("w3cos_electron_port_transfer", PORT_TRANSFER);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_port_transfer",
        &[],
    );
    assert_eq!(
        lines,
        ["renderer ports 1", "renderer got pong:ping:child"],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Code-OSS forks `bootstrap-fork.js` (an ES module) through a computed
/// path and imports its entry point with a computed specifier: the app
/// declares both in `package.json` (`"w3cos": { "workers", "modules" }`).
const ESM_UTILITY: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-esm-utility", "version": "0.1.0", "type": "module", "main": "main.js",
  "w3cos": { "workers": ["child.js"], "modules": ["lazy/feature.js"] } }"#,
    ),
    (
        "main.js",
        r#"import { app, utilityProcess } from 'electron'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { shared } from './shared.js'
const dir = path.dirname(fileURLToPath(import.meta.url))
const name = ['child', 'js'].join('.')
app.whenReady().then(() => {
  const child = utilityProcess.fork(path.join(dir, name), ['feature'], { env: { ...process.env, W3COS_FORK_ENTRY: 'lazy' } })
  child.on('message', (message) => {
    console.log('main got', message, shared, typeof process.env.W3COS_FORK_ENTRY)
    child.kill()
  })
  child.on('exit', () => app.quit())
  child.postMessage('hi')
})
"#,
    ),
    (
        "shared.js",
        r#"export const shared = 'shared'
console.log('shared evaluated in', process.parentPort ? 'child' : 'main')
"#,
    ),
    (
        "child.js",
        r#"import { shared } from './shared.js'
const which = process.argv[2]
// Like Code-OSS's bootstrap-fork.js: the entry comes from the fork's env.
const feature = await import(`./${process.env.W3COS_FORK_ENTRY}/${which}.js`)
process.parentPort.on('message', (event) => {
  process.parentPort.postMessage(shared + ':' + feature.describe(event.data))
})
"#,
    ),
    (
        "lazy/feature.js",
        r#"export function describe(data) { return 'feature(' + data + ')' }
"#,
    ),
];

#[test]
fn electron_declared_esm_utility_process_and_computed_import() {
    let root = write_fixture("w3cos_electron_esm_utility", ESM_UTILITY);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_esm_utility",
        &[],
    );
    assert_eq!(
        lines,
        [
            "shared evaluated in main",
            "shared evaluated in child",
            "main got shared:feature(hi) shared undefined",
        ],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Code-OSS posts the shared process's configuration right after
/// `utilityProcess.fork()`; the child only listens (`parentPort.once`) once
/// its entry module has loaded. Electron's ParentPort queues messages until a
/// 'message' listener is added and pauses again when the last one goes.
const PARENT_PORT_QUEUE: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-electron-parent-port-queue", "version": "0.1.0", "main": "main.js" }"#,
    ),
    (
        "main.js",
        r#"const { app, utilityProcess } = require('electron')
const path = require('node:path')
app.whenReady().then(() => {
  const child = utilityProcess.fork(path.join(__dirname, 'child.js'))
  child.postMessage('first')
  child.postMessage('second')
  child.on('message', (message) => {
    console.log('main got', message)
    if (message === 'done') child.kill()
  })
  child.on('exit', () => app.quit())
})
"#,
    ),
    (
        "child.js",
        r#"setTimeout(() => {
  process.parentPort.once('message', (event) => {
    process.parentPort.postMessage('once ' + event.data)
    setTimeout(() => {
      process.parentPort.on('message', (later) => {
        process.parentPort.postMessage('on ' + later.data)
        process.parentPort.postMessage('done')
      })
    }, 20)
  })
}, 20)
"#,
    ),
];

#[test]
fn electron_parent_port_queues_messages_until_a_listener_is_added() {
    let root = write_fixture("w3cos_electron_parent_port_queue", PARENT_PORT_QUEUE);
    let lines = compile_and_run_with_env(
        &root,
        &root.join("main.js"),
        "w3cos_electron_parent_port_queue",
        &[],
    );
    assert_eq!(
        lines,
        ["main got once first", "main got on second", "main got done"],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}
