# `navigator.w3cos` Explainer

Status: w3cos extension. Not a W3C or WICG deliverable.
This note is written so the shapes can be proposed upstream later.

## Motivation

A set of native capabilities have no Web IDL draft: spawning a process,
talking to it over IPC, a system tray, global shortcuts, managing other
apps' windows, background daemons and login autostart, writing system
settings and power actions, a generic secret store, and reading another
app's accessibility tree or capturing the screen without a picker.

Standards that do exist stay on their standard objects (`TCPSocket`,
`navigator.ml`, `getScreenDetails`, `navigator.getBattery`,
`navigator.wakeLock`, `IdleDetector`, `getDisplayMedia`, and the File
System Access pickers). This namespace is only for the gap.

## Why `navigator.w3cos`

The name sits on `navigator` next to other capability surfaces, and the
`w3cos` member makes the non-standard boundary obvious to feature
detection:

```js
if (navigator.w3cos?.process) {
  // extension, not a W3C API
}
```

It is not a root `Navigator` member (`navigator.process`), not a global
`w3cos`, and not `chrome.*` or `electron.*`. Those names either look like
a standard API or import another vendor's permission model.

## Permission model

Every method checks one named capability before it touches the host.
`prompt` and `denied` both reject `NotAllowedError`. Only an explicit
`granted` decision runs the call. A granted call that has no platform
adapter rejects `NotSupportedError` with a message that says there is no
draft and no adapter. The implementation does not return an empty success.

Page names that already exist on the AI Bridge agent reuse that agent's
bits. Hosts must set both; the runtime does not import the bridge crate.

| Page name | Bridge field | Meaning |
|---|---|---|
| `w3cos-accessibility` | `can_read_dom` | Read the current app's accessibility tree |
| `display-capture` | `can_screenshot` | Capture this app's display |
| `w3cos-process` | `can_spawn_processes` | Spawn a process or open IPC |
| `file-system` | `can_access_files` | File System Access directory grant |

`AgentPermissions::grants_page_capability` is the mapping. Other extension
names (`w3cos-global-shortcuts`, `w3cos-tray`, `w3cos-window-manager`,
`w3cos-daemon`, `w3cos-system`, `w3cos-keyring`) have no bridge bit yet.

Suggested prompt copy:

- **Process and IPC** — “Allow this app to start programs and exchange messages with them?”
- **Global shortcuts** — “Allow this app to register a system-wide keyboard shortcut?”
- **Tray** — “Allow this app to add a status-item and menu?”
- **Window manager** — “Allow this app to list and restack other windows?”
- **Daemon** — “Allow this app to keep running after its window closes and to start at login?”
- **System** — “Allow this app to read or change system settings and power actions?”
- **Keyring** — “Allow this app to store a secret in the system keyring?”
- **Accessibility** — “Allow this app to read its own accessibility tree?”
- **Display capture** — the existing screen-capture prompt. A request whose `scope` is `system`, `cross-app`, or `background` is refused even after that grant.

## API shape

```webidl
[Exposed=Window]
interface W3cos {
  readonly attribute W3cosProcess process;
  readonly attribute W3cosIpc ipc;
  readonly attribute W3cosShortcuts shortcuts;
  readonly attribute W3cosTray tray;
  readonly attribute W3cosWindows windows;
  readonly attribute W3cosDaemon daemon;
  readonly attribute W3cosSystem system;
  readonly attribute W3cosKeyring keyring;
  readonly attribute W3cosAgent agent;
};

dictionary W3cosSpawnOptions {
  required USVString file;
  sequence<USVString> args;
  USVString cwd;
};

[Exposed=Window]
interface W3cosProcess {
  Promise<W3cosProcessHandle> spawn(W3cosSpawnOptions options);
  Promise<W3cosExecResult> exec(W3cosSpawnOptions options);
};

[Exposed=Window]
interface W3cosProcessHandle {
  readonly attribute unsigned long pid;
  Promise<undefined> writeStdin(USVString data);
  undefined closeStdin();
  Promise<undefined> kill();
  Promise<W3cosExecResult> wait();
};

dictionary W3cosExecResult {
  USVString stdout;
  USVString stderr;
  long exitCode;
  boolean ok;
};

[Exposed=Window]
interface W3cosIpc {
  Promise<W3cosIpcServer> listen();
  Promise<W3cosIpcPort> connect(USVString endpoint);
};

[Exposed=Window]
interface W3cosIpcServer {
  readonly attribute USVString endpoint;
  Promise<W3cosIpcMessage?> receive();
};

[Exposed=Window]
interface W3cosIpcPort {
  readonly attribute USVString endpoint;
  Promise<undefined> postMessage(USVString channel, USVString payload);
};

dictionary W3cosIpcMessage {
  USVString channel;
  USVString payload;
};

dictionary W3cosShortcutOptions {
  boolean global = true;
};

[Exposed=Window]
interface W3cosShortcuts {
  Promise<W3cosShortcutHandle> register(
    USVString accelerator,
    EventHandler callback,
    optional W3cosShortcutOptions options = {}
  );
};

[Exposed=Window]
interface W3cosShortcutHandle {
  readonly attribute USVString accelerator;
  readonly attribute USVString scope; // "system" or "application"
  undefined unregister();
};

dictionary W3cosTrayMenuItem {
  USVString id;
  USVString label;
  USVString type; // "separator" marks a separator; id and label are then omitted
  boolean enabled = true;
  boolean checked = false;
  boolean separator = false;
};

dictionary W3cosTrayIcon {
  required unsigned long width;
  required unsigned long height;
  required sequence<octet> rgba;
};

dictionary W3cosTrayOptions {
  USVString tooltip;
  USVString title;
  sequence<W3cosTrayMenuItem> menu;
  W3cosTrayIcon icon;
};

[Exposed=Window]
interface W3cosTray {
  Promise<W3cosTrayHandle> show(optional W3cosTrayOptions options = {});
  Promise<undefined> setMenu(sequence<W3cosTrayMenuItem> menu);
};

[Exposed=Window]
interface W3cosTrayHandle {
  readonly attribute unsigned long long id;
  readonly attribute USVString tooltip;
  readonly attribute USVString title;
  readonly attribute boolean removed;
  Promise<undefined> update(optional W3cosTrayOptions options = {});
  Promise<undefined> setMenu(sequence<W3cosTrayMenuItem> menu);
  Promise<undefined> onClick(EventHandler callback);
  Promise<undefined> onMenu(EventHandler callback);
  Promise<undefined> remove();
};

[Exposed=Window]
interface W3cosKeyring {
  Promise<USVString?> get(USVString name);
  Promise<undefined> set(USVString name, USVString secret);
  Promise<undefined> delete(USVString name);
  Promise<sequence<USVString>> list();
};

dictionary W3cosAgentOptions {
  USVString scope = "app"; // "system" | "cross-app" | "background" are refused
};

[Exposed=Window]
interface W3cosAgent {
  Promise<USVString> accessibilityTree(optional W3cosAgentOptions options = {});
  Promise<W3cosCapture> captureScreen(optional W3cosAgentOptions options = {});
};
```

`windows`, `daemon`, and `system` expose their method names
(`list`, `focus`, `setAlwaysOnTop`, `install`, `start`, `enableAutostart`,
`get`, `set`, `power`) so callers can feature-detect them. Each one
rejects. `tray.show`, `tray.setMenu`, and the `keyring` methods are real.

### Implemented slice

- `process.spawn` / `process.exec` start a program with piped stdio. `exec`
  waits and returns stdout, stderr, and the exit code. The shell string
  form is not exposed.
- `ipc.listen` binds a unique Unix socket. `connect` and `postMessage`
  send a channel plus a string. `receive` polls for a short interval and
  resolves `null` on timeout.
- `shortcuts.register(accelerator, callback, { global })`. The default
  `global: true` installs an OS grab (Linux: `XGrabKey` on a side thread)
  and rejects `NotSupportedError` if the grab fails. `global: false` is
  application scope and is delivered from the window keyboard path only
  when a shortcut is registered, so an empty registry does not touch the
  input hot path. The OS thread queues accelerator strings;
  `flush_global_shortcuts()` delivers them on the JavaScript thread.
- `navigator.w3cos.tray` and `navigator.w3cos.keyring` stay available as
  explicit `w3cos-runtime` features. `host-tray` and `host-keyring` are in
  the crate `default` set and in `w3cos-shell`
  (`features = ["web-sensors", "web-geolocation", "web-bluetooth", "host-tray", "host-keyring", "image-png", "image-jpeg", "image-webp", "image-gif", "image-bmp", "image-ico"]`).
  Ordinary AOT, the size probes, and generated apps use
  `default-features = false` and leave both off, so the call rejects
  `NotSupportedError` and does not link `ksni`, `tray-icon`, or `keyring`.
  Turn them back on for a generated app with
  `W3COS_RUNTIME_FEATURES=host-tray,host-keyring`, or by adding those names
  to the runtime `features` list.
- `tray.show({ tooltip, title, menu, icon })` installs a status item and
  returns a handle. `update` changes that item. `setMenu` replaces its
  menu; the namespace method updates the most recently shown icon that is
  still installed. `onClick` and `onMenu` register callbacks. `remove`
  drops the item. Tooltip, title, ids, and labels are limited to 128
  characters. Menus are a flat list of at most 40 items; a `submenu`
  rejects `TypeError`. Icon pixels are RGBA, at most 128×128; omitted
  pixels use a built-in 16×16 icon. The shell calls
  `flush_tray_events()` on the JavaScript thread. A missing permission is
  `NotAllowedError`. A missing session, or a target with no adapter,
  rejects `NotSupportedError` and does not report success.
- Linux calls the StatusNotifierItem D-Bus interface through `ksni` (the
  protocol Ayatana AppIndicator implements). This build does not link GTK
  or `libappindicator`. A panel that watches StatusNotifierItem, including
  KDE and GNOME's AppIndicator extension, can display the item. No session
  bus is an adapter failure.
- macOS calls `NSStatusItem`. The icon is created on the thread that calls
  `show`, which has to be the main thread, and the shell event loop has to
  be running. `title` is the status-item title. Left click is the click
  callback; the menu is the context menu.
- Windows calls `Shell_NotifyIcon`. The creating thread has to pump Win32
  messages; the shell loop does. `title` is ignored by the shell. Left
  click is the click callback; right click opens the menu. Tooltip text
  follows the 128-character limit above.
- `keyring.get` / `keyring.set` / `keyring.delete` / `keyring.list` read
  and write the current document origin's secrets. The service name is
  derived from that origin. `list` returns names only, and only for items
  whose stored service matches this origin exactly, so a prefix of another
  origin is not enough. `get` resolves `null` when the name is absent.
  `delete` of a missing name resolves. The desktop adapter is the `keyring`
  crate: Linux talks to the Secret Service over D-Bus (the same service
  libsecret uses), macOS uses Keychain Services, and Windows uses
  Credential Manager. Android, iOS, and HarmonyOS have no adapter yet.
  A missing session bus, a locked store, or any other failure to open the
  backend rejects `NotSupportedError`. Nothing is written to a file
  instead. Exception text does not include the secret.

### Refused until an adapter exists

- `windows.list` / `windows.focus` / `windows.setAlwaysOnTop`
- `daemon.install` / `daemon.start` / `daemon.enableAutostart`
- `system.get` / `system.set` / `system.power`
- `agent.accessibilityTree({ scope: "cross-app" })` and
  `agent.captureScreen({ scope: "background" })`

`agent.accessibilityTree()` without a foreign scope returns the current
document's accessibility tree, the same builder the AI Bridge uses.
`agent.captureScreen()` returns `{ width, height, blob }` from the same
display capture as `getDisplayMedia`.

## Security and privacy

- Process spawn can run any executable the OS user can run. The prompt is
  not a sandbox. A future isolated-app mode should restrict `file` to a
  declared set.
- IPC endpoints are local sockets created by this process. They are not a
  network listener. Payloads are strings in this slice.
- A global shortcut grabs a key combination for every application. The
  prompt must name the accelerator. Failed grabs do not fall back to a
  silent in-app registration.
- A tray icon is visible in the session status area and can open a menu.
  The prompt must name the app. The adapter does not read other apps'
  status items. Menu labels are plain text. Realm reset removes every icon
  this realm created. Callbacks run only after `flush_tray_events()` on
  the JavaScript thread; a failed adapter does not invent a click.
- The accessibility tree and the capture blob describe the current app
  only. Cross-app and unprompted background capture stay refused so this
  namespace cannot bypass the bridge's screenshot and DOM-read bits.
- The keyring requires `w3cos-keyring` before it touches the store. Items
  are scoped to the document origin (`w3cos://files` and `w3cos://editor`
  do not see each other). Hosts that leave every app on the default
  `w3cos://app` origin share that one namespace. Secrets are not written
  to logs or exception messages. There is no in-memory fallback when the
  system store cannot be opened; headless CI without a Secret Service
  session gets `NotSupportedError`.
- System power and settings are refused. A later `power("shutdown")` needs
  its own confirmation, not only the capability bit.

## Out of scope for this note

Window Controls Overlay, WebUSB, WebHID, Web Serial, Web Bluetooth, local
fonts, sensors, compute pressure, WebGPU, EyeDropper, Web Share, Media
Session, Contact Picker, WebNFC, and Isolated Web Apps already have
drafts. Finer-grained printing and smart-card access are also uncovered
here; they are listed in the gap report and are not stubbed on
`navigator.w3cos` yet.
