'use strict';
// `electron` / `@w3cos/electron`: Electron's main- and renderer-process API
// over one AOT-compiled w3cos process (M2, docs/W3COS_ELECTRON.md).
//
// The main script, its preloads and its page scripts are compiled together;
// the renderer is the process's own DOM (`document`, `window`), so the first
// BrowserWindow owns the w3cos window. IPC is in-process message passing
// with Electron's ordering (every message is a separate task) and
// structured-clone copies. Native affordances use the standard web objects
// where a W3C standard or draft exists (File System Access pickers, `alert`,
// `confirm`, `window.open`), never a global `electron` or `w3cos` object.

const EventEmitter = require('events');
const path = require('path');
const os = require('os');
const process = require('process');
const assets = require('./electron_app.js');

// The Electron API level this adapter is tested against (the reference
// outputs in tests were recorded with Electron 44.9.0). There is no
// Chromium, so `process.versions.chrome` stays unset.
const ELECTRON_VERSION = '44.9.0';
if (process.versions.electron === undefined) process.versions.electron = ELECTRON_VERSION;

const env = process.env || {};
const TRACE = env.W3COS_ELECTRON_TRACE === '1';
const EXIT_AFTER_LOAD = env.W3COS_ELECTRON_EXIT_AFTER_LOAD === '1';

function trace(message) {
  if (TRACE) console.log('[electron] ' + message);
}

// One task per message, like Electron's IPC and navigation.
function task(callback) {
  setTimeout(callback, 0);
}

function clone(value) {
  if (value === undefined) return undefined;
  if (typeof structuredClone === 'function') return structuredClone(value);
  return JSON.parse(JSON.stringify(value));
}

function cloneArgs(args) {
  const out = [];
  for (let i = 0; i < args.length; i++) out.push(clone(args[i]));
  return out;
}

function makeEvent(fields) {
  const event = {
    defaultPrevented: false,
    preventDefault() { event.defaultPrevented = true; },
  };
  if (fields) Object.assign(event, fields);
  return event;
}

function unsupported(what) {
  const error = new Error(what + ' is not supported by this w3cos runtime');
  error.name = 'NotSupportedError';
  error.code = 'ERR_W3COS_UNSUPPORTED';
  return error;
}

function hasDom() {
  return typeof document === 'object' && document !== null && typeof window === 'object' && window !== null;
}

// --- app -------------------------------------------------------------------

const windows = [];
let nextWindowId = 1;
let nextWebContentsId = 1;
let resolveReady;
const readyPromise = new Promise((resolve) => { resolveReady = resolve; });
let appName = assets.name || 'Electron';
const appVersion = assets.version || ELECTRON_VERSION;

class App extends EventEmitter {
  constructor() {
    super();
    this._ready = false;
    this._quitting = false;
    this._quitted = false;
    this.isPackaged = true;
    this.commandLine = {
      appendSwitch() {},
      appendArgument() {},
      hasSwitch() { return false; },
      getSwitchValue() { return ''; },
      removeSwitch() {},
    };
  }
  get name() { return appName; }
  set name(value) { appName = String(value); }
  isReady() { return this._ready; }
  whenReady() { return readyPromise; }
  getName() { return appName; }
  setName(value) { appName = String(value); }
  getVersion() { return appVersion; }
  getAppPath() { return assets.root; }
  getLocale() {
    return typeof navigator === 'object' && navigator !== null && navigator.language ? navigator.language : 'en-US';
  }
  getSystemLocale() { return this.getLocale(); }
  getPreferredSystemLanguages() { return [this.getLocale()]; }
  getPath(name) {
    const home = os.homedir();
    const appData = process.platform === 'darwin'
      ? path.join(home, 'Library', 'Application Support')
      : process.platform === 'win32'
        ? (env.APPDATA || path.join(home, 'AppData', 'Roaming'))
        : (env.XDG_CONFIG_HOME || path.join(home, '.config'));
    const userData = path.join(appData, appName);
    const paths = {
      home,
      appData,
      userData,
      sessionData: userData,
      temp: os.tmpdir(),
      exe: process.execPath,
      module: process.execPath,
      desktop: path.join(home, 'Desktop'),
      documents: path.join(home, 'Documents'),
      downloads: path.join(home, 'Downloads'),
      music: path.join(home, 'Music'),
      pictures: path.join(home, 'Pictures'),
      videos: path.join(home, 'Videos'),
      logs: path.join(userData, 'logs'),
      crashDumps: path.join(userData, 'Crashpad'),
    };
    if (!Object.prototype.hasOwnProperty.call(paths, name)) {
      throw new Error("Failed to get '" + name + "' path");
    }
    return paths[name];
  }
  requestSingleInstanceLock() { return true; }
  hasSingleInstanceLock() { return true; }
  releaseSingleInstanceLock() {}
  setAppUserModelId() {}
  setAboutPanelOptions() {}
  disableHardwareAcceleration() {}
  focus() {}
  hide() {}
  show() {}
  quit() {
    if (this._quitting || this._quitted) return;
    const before = makeEvent();
    this.emit('before-quit', before);
    if (before.defaultPrevented) return;
    this._quitting = true;
    for (const win of windows.slice()) win.close();
    if (windows.length > 0) {
      // A window vetoed its `close`: the quit is cancelled.
      this._quitting = false;
      return;
    }
    finishQuit(0);
  }
  exit(code) {
    this._quitted = true;
    for (const win of windows.slice()) win.destroy();
    process.exit(code === undefined ? 0 : code);
  }
  relaunch() { throw unsupported('app.relaunch()'); }
}

const app = new App();

function finishQuit(exitCode) {
  const willQuit = makeEvent();
  trace('will-quit');
  app.emit('will-quit', willQuit);
  if (willQuit.defaultPrevented) {
    app._quitting = false;
    return;
  }
  app._quitting = false;
  app._quitted = true;
  app.emit('quit', makeEvent(), exitCode);
  task(() => process.exit(exitCode));
}

// `ready` once the main script has finished its synchronous part.
task(() => {
  app._ready = true;
  app.emit('ready', makeEvent(), {});
  resolveReady();
});

// --- renderer realm ----------------------------------------------------------

// The single renderer realm (the process's DOM) and the webContents that
// currently owns it; preloads and page scripts run against it.
let hostContents = null;

function moduleKey(file) {
  const absolute = path.resolve(assets.root, String(file));
  const relative = path.relative(assets.root, absolute);
  if (!relative.startsWith('..')) return relative;
  for (const key of assets.modules) {
    if (absolute.endsWith('/' + key)) return key;
  }
  return relative;
}

function runRendererScript(file, label) {
  const key = moduleKey(file);
  if (assets.modules.indexOf(key) < 0) {
    console.error('Unable to load ' + label + ' ' + file + ': not compiled into this app');
    return;
  }
  try {
    assets.load(key);
  } catch (error) {
    console.error(label === 'preload'
      ? 'Unable to load preload script: ' + file
      : 'Uncaught ' + (error && error.stack ? error.stack : error));
    if (label === 'preload') console.error(error);
  }
}

function renderPage(contents, page) {
  document.head.innerHTML = page.head;
  document.body.innerHTML = page.body;
  document.title = page.title;
  const preload = contents._preferences.preload;
  if (preload) runRendererScript(preload, 'preload');
  for (const script of page.scripts) runRendererScript(script, 'script');
  document.dispatchEvent(new Event('DOMContentLoaded', { bubbles: true }));
}

// --- webContents -------------------------------------------------------------

class WebContents extends EventEmitter {
  constructor(win, preferences) {
    super();
    this.id = nextWebContentsId++;
    this._win = win;
    this._preferences = preferences || {};
    this._url = '';
    this._loading = false;
    this._destroyed = false;
    this._windowOpenHandler = null;
    this.session = { clearCache() { return Promise.resolve(); }, setPermissionRequestHandler() {} };
  }
  getURL() { return this._url; }
  getTitle() {
    if (this === hostContents && hasDom()) return document.title;
    return this._win ? this._win._title : '';
  }
  isLoading() { return this._loading; }
  isDestroyed() { return this._destroyed; }
  getOwnerBrowserWindow() { return this._win; }
  loadFile(file, options) {
    const absolute = path.resolve(assets.root, String(file));
    let url = 'file://' + absolute;
    if (options && options.query) {
      const query = Object.keys(options.query)
        .map((key) => encodeURIComponent(key) + '=' + encodeURIComponent(options.query[key]))
        .join('&');
      if (query) url += '?' + query;
    }
    if (options && options.search) url += '?' + String(options.search).replace(/^\?/, '');
    if (options && options.hash) url += '#' + String(options.hash).replace(/^#/, '');
    return this._load(url, path.relative(assets.root, absolute));
  }
  loadURL(url) {
    const href = String(url);
    if (href.startsWith('file://')) {
      const file = decodeURIComponent(href.slice('file://'.length).split(/[?#]/)[0]);
      return this._load(href, path.relative(assets.root, file));
    }
    return this._load(href, null);
  }
  reload() { if (this._url) this.loadURL(this._url); }
  _load(url, pageKey) {
    return new Promise((resolve, reject) => {
      task(() => {
        if (this._destroyed) {
          reject(new Error('ERR_ABORTED (-3) loading \'' + url + '\''));
          return;
        }
        const page = pageKey === null ? undefined : assets.pages[pageKey];
        const fail = (code, description) => {
          this.emit('did-fail-load', makeEvent(), code, description, url, true);
          const error = new Error(description + ' (' + code + ') loading \'' + url + '\'');
          error.code = description;
          error.errno = code;
          error.url = url;
          reject(error);
        };
        if (page === undefined) {
          // Remote pages need a network-loaded document; only the app's own
          // compiled pages can be shown.
          fail(pageKey === null ? -301 : -6, pageKey === null ? 'ERR_DISALLOWED_URL_SCHEME' : 'ERR_FILE_NOT_FOUND');
          return;
        }
        if (hostContents === null) hostContents = this;
        if (hostContents !== this || !hasDom()) {
          fail(-2, 'ERR_FAILED');
          console.error('w3cos: one renderer per process; window ' + this._win.id + ' cannot show a page');
          return;
        }
        this._url = url;
        this._loading = true;
        this.emit('did-start-loading');
        renderPage(this, page);
        this.emit('dom-ready', makeEvent());
        window.dispatchEvent(new Event('load'));
        this._loading = false;
        this.emit('did-frame-finish-load', makeEvent(), true, 1, 1);
        this.emit('did-finish-load', makeEvent());
        this.emit('did-stop-loading', makeEvent());
        trace('page-loaded ' + JSON.stringify(document.title) + ' ' +
          JSON.stringify(String(document.body.textContent).replace(/\s+/g, ' ').trim()));
        resolve();
        if (EXIT_AFTER_LOAD) task(() => { if (this._win && !this._win.isDestroyed()) this._win.close(); });
      });
    });
  }
  send(channel, ...args) {
    if (this._destroyed) return;
    const copies = cloneArgs(args);
    task(() => {
      if (this._destroyed || this !== hostContents) return;
      ipcRenderer.emit(String(channel), makeEvent({ sender: ipcRenderer, senderId: 0, ports: [] }), ...copies);
    });
  }
  postMessage(channel, message) { this.send(channel, message); }
  executeJavaScript() {
    // No `eval` in an AOT-compiled app: renderer code must be compiled in.
    return Promise.reject(unsupported('webContents.executeJavaScript()'));
  }
  insertCSS(css) {
    if (this === hostContents && hasDom()) {
      const style = document.createElement('style');
      style.textContent = String(css);
      document.head.appendChild(style);
    }
    return Promise.resolve('');
  }
  setWindowOpenHandler(handler) { this._windowOpenHandler = handler; }
  openDevTools() {}
  closeDevTools() {}
  isDevToolsOpened() { return false; }
  toggleDevTools() {}
  focus() {}
  setZoomFactor() {}
  getZoomFactor() { return 1; }
}

// --- BrowserWindow -----------------------------------------------------------

class BrowserWindow extends EventEmitter {
  constructor(options) {
    super();
    const opts = options || {};
    this.id = nextWindowId++;
    this._bounds = {
      x: opts.x === undefined ? 0 : opts.x,
      y: opts.y === undefined ? 0 : opts.y,
      width: opts.width === undefined ? 800 : opts.width,
      height: opts.height === undefined ? 600 : opts.height,
    };
    this._title = opts.title === undefined ? appName : String(opts.title);
    this._visible = opts.show !== false;
    this._destroyed = false;
    this._state = 'normal';
    this.webContents = new WebContents(this, opts.webPreferences);
    windows.push(this);
    trace('window-created ' + this._bounds.width + 'x' + this._bounds.height);
    app.emit('browser-window-created', makeEvent(), this);
  }
  static getAllWindows() { return windows.slice(); }
  static getFocusedWindow() { return windows.length > 0 ? windows[0] : null; }
  static fromId(id) { return windows.find((win) => win.id === id) || null; }
  static fromWebContents(contents) {
    return windows.find((win) => win.webContents === contents) || null;
  }
  _isHost() { return this.webContents === hostContents && hasDom(); }
  getTitle() { return this.webContents.getTitle(); }
  setTitle(title) {
    this._title = String(title);
    if (this._isHost()) document.title = this._title;
  }
  getBounds() { return Object.assign({}, this._bounds); }
  setBounds(bounds) { Object.assign(this._bounds, bounds); this.emit('resize'); }
  getContentBounds() { return this.getBounds(); }
  setContentBounds(bounds) { this.setBounds(bounds); }
  getSize() { return [this._bounds.width, this._bounds.height]; }
  setSize(width, height) { this.setBounds({ width, height }); }
  getContentSize() { return this.getSize(); }
  setContentSize(width, height) { this.setSize(width, height); }
  getPosition() { return [this._bounds.x, this._bounds.y]; }
  setPosition(x, y) { this._bounds.x = x; this._bounds.y = y; this.emit('move'); }
  center() {}
  setMinimumSize() {}
  setMaximumSize() {}
  setResizable() {}
  isResizable() { return true; }
  show() { this._visible = true; this.emit('show'); }
  showInactive() { this.show(); }
  hide() { this._visible = false; this.emit('hide'); }
  isVisible() { return this._visible && !this._destroyed && this._state !== 'minimized'; }
  focus() { this.emit('focus'); }
  blur() { this.emit('blur'); }
  isFocused() { return this._visible && windows[0] === this; }
  minimize() { this._state = 'minimized'; this.emit('minimize'); }
  restore() { this._state = 'normal'; this.emit('restore'); }
  isMinimized() { return this._state === 'minimized'; }
  maximize() { this._state = 'maximized'; this.emit('maximize'); }
  unmaximize() { this._state = 'normal'; this.emit('unmaximize'); }
  isMaximized() { return this._state === 'maximized'; }
  setFullScreen(flag) { this._state = flag ? 'fullscreen' : 'normal'; }
  isFullScreen() { return this._state === 'fullscreen'; }
  setMenu() {}
  removeMenu() {}
  setMenuBarVisibility() {}
  setAutoHideMenuBar() {}
  setIcon() {}
  setProgressBar() {}
  isDestroyed() { return this._destroyed; }
  loadFile(file, options) { return this.webContents.loadFile(file, options); }
  loadURL(url, options) { return this.webContents.loadURL(url, options); }
  reload() { this.webContents.reload(); }
  close() {
    if (this._destroyed) return;
    const event = makeEvent();
    this.emit('close', event);
    if (event.defaultPrevented) return;
    this.destroy();
  }
  destroy() {
    if (this._destroyed) return;
    this._destroyed = true;
    const index = windows.indexOf(this);
    if (index >= 0) windows.splice(index, 1);
    const contents = this.webContents;
    contents._destroyed = true;
    if (contents === hostContents) hostContents = null;
    contents.emit('destroyed');
    this.emit('closed');
    if (windows.length === 0 && !app._quitted) {
      if (app._quitting) return;
      trace('window-all-closed');
      if (app.listenerCount('window-all-closed') > 0) app.emit('window-all-closed');
      else app.quit();
    }
  }
}

// --- IPC -----------------------------------------------------------------------

const handlers = new Map();

class IpcMain extends EventEmitter {
  handle(channel, listener) {
    const name = String(channel);
    if (handlers.has(name)) {
      throw new Error("Attempted to register a second handler for '" + name + "'");
    }
    handlers.set(name, listener);
  }
  handleOnce(channel, listener) {
    const name = String(channel);
    this.handle(name, (...args) => {
      handlers.delete(name);
      return listener(...args);
    });
  }
  removeHandler(channel) { handlers.delete(String(channel)); }
}

const ipcMain = new IpcMain();

function mainEvent(contents) {
  return makeEvent({
    sender: contents,
    senderFrame: { url: contents ? contents.getURL() : '', processId: 1, routingId: 1 },
    frameId: 1,
    processId: 1,
    ports: [],
    returnValue: undefined,
    reply(channel, ...args) { if (contents) contents.send(channel, ...args); },
  });
}

function rendererContents() {
  if (hostContents === null) throw new Error('ipcRenderer is only available to a loaded page');
  return hostContents;
}

class IpcRenderer extends EventEmitter {
  send(channel, ...args) {
    const contents = rendererContents();
    const copies = cloneArgs(args);
    task(() => ipcMain.emit(String(channel), mainEvent(contents), ...copies));
  }
  invoke(channel, ...args) {
    const name = String(channel);
    let contents;
    try {
      contents = rendererContents();
    } catch (error) {
      return Promise.reject(error);
    }
    const copies = cloneArgs(args);
    return new Promise((resolve, reject) => {
      task(() => {
        const handler = handlers.get(name);
        const fail = (error) => reject(new Error("Error invoking remote method '" + name + "': " + error));
        if (handler === undefined) {
          fail(new Error("No handler registered for '" + name + "'"));
          return;
        }
        Promise.resolve()
          .then(() => handler(mainEvent(contents), ...copies))
          .then((result) => resolve(clone(result)), fail);
      });
    });
  }
  sendSync(channel, ...args) {
    const event = mainEvent(rendererContents());
    ipcMain.emit(String(channel), event, ...cloneArgs(args));
    return clone(event.returnValue);
  }
  sendToHost(channel, ...args) { this.send(channel, ...args); }
  postMessage(channel, message) { this.send(channel, message); }
}

const ipcRenderer = new IpcRenderer();

// --- contextBridge -------------------------------------------------------------

// The renderer world and the preload share one realm here, so the bridge is
// a copy of the API object (functions wrapped, objects frozen), as Electron
// hands the page.
function project(value, depth) {
  if (typeof value === 'function') {
    const fn = value;
    return function bridged(...args) { return fn(...args); };
  }
  if (value === null || typeof value !== 'object' || depth > 32) return value;
  if (value instanceof Promise || value instanceof Date || value instanceof RegExp || ArrayBuffer.isView(value)) {
    return value;
  }
  if (Array.isArray(value)) return Object.freeze(value.map((item) => project(item, depth + 1)));
  const out = {};
  for (const key of Object.keys(value)) out[key] = project(value[key], depth + 1);
  return Object.freeze(out);
}

const contextBridge = {
  exposeInMainWorld(key, api) {
    const name = String(key);
    if (!hasDom()) throw new Error('contextBridge is only available to a preload script');
    if (window[name] !== undefined) {
      throw new Error('Cannot bind an API on top of an existing property on the window object');
    }
    window[name] = project(api, 0);
  },
  exposeInIsolatedWorld(worldId, key, api) { contextBridge.exposeInMainWorld(key, api); },
  executeInMainWorld(script) {
    const args = script && script.args ? script.args : [];
    return script.func(...args);
  },
};

// --- dialog (File System Access pickers, alert/confirm) -----------------------

function pickerFailed(error) {
  // A dismissed or refused picker is a cancelled dialog.
  const name = error && error.name;
  if (name === 'AbortError' || name === 'NotAllowedError' || name === 'SecurityError' || name === 'NotSupportedError') {
    return true;
  }
  throw error;
}

function handlePath(handle) {
  return typeof handle.path === 'string' ? handle.path : handle.name;
}

function dialogOptions(win, options) {
  return win instanceof BrowserWindow || options !== undefined ? (options || {}) : (win || {});
}

const dialog = {
  showOpenDialog(win, options) {
    const opts = dialogOptions(win, options);
    const properties = opts.properties || ['openFile'];
    if (!hasDom()) return Promise.resolve({ canceled: true, filePaths: [] });
    const directory = properties.indexOf('openDirectory') >= 0;
    let picked;
    try {
      picked = directory
        ? window.showDirectoryPicker().then((handle) => [handle])
        : window.showOpenFilePicker({ multiple: properties.indexOf('multiSelections') >= 0 });
    } catch (error) {
      picked = Promise.reject(error);
    }
    return Promise.resolve(picked).then(
      (handles) => ({ canceled: false, filePaths: handles.map(handlePath), handles }),
      (error) => pickerFailed(error) && { canceled: true, filePaths: [] },
    );
  },
  showSaveDialog(win, options) {
    const opts = dialogOptions(win, options);
    if (!hasDom()) return Promise.resolve({ canceled: true, filePath: '' });
    const suggestedName = opts.defaultPath ? path.basename(opts.defaultPath) : undefined;
    let picked;
    try {
      picked = window.showSaveFilePicker({ suggestedName });
    } catch (error) {
      picked = Promise.reject(error);
    }
    return Promise.resolve(picked).then(
      (handle) => ({ canceled: false, filePath: handlePath(handle), handle }),
      (error) => pickerFailed(error) && { canceled: true, filePath: '' },
    );
  },
  showOpenDialogSync() { return undefined; },
  showSaveDialogSync() { return undefined; },
  showMessageBoxSync(win, options) {
    const opts = dialogOptions(win, options);
    const text = [opts.title, opts.message, opts.detail].filter((part) => part).join('\n\n');
    const buttons = opts.buttons || [];
    if (!hasDom()) return opts.cancelId === undefined ? 0 : opts.cancelId;
    if (buttons.length <= 1) {
      window.alert(text);
      return 0;
    }
    const ok = window.confirm(text);
    const defaultId = opts.defaultId === undefined ? 0 : opts.defaultId;
    const cancelId = opts.cancelId === undefined ? (defaultId === 0 ? 1 : 0) : opts.cancelId;
    return ok ? defaultId : cancelId;
  },
  showMessageBox(win, options) {
    const response = dialog.showMessageBoxSync(win, options);
    return Promise.resolve({ response, checkboxChecked: false });
  },
  showErrorBox(title, content) {
    if (hasDom()) window.alert(String(title) + '\n\n' + String(content));
    else console.error(String(title) + '\n' + String(content));
  },
  showCertificateTrustDialog() { return Promise.reject(unsupported('dialog.showCertificateTrustDialog()')); },
};

// --- shell ---------------------------------------------------------------------

const shell = {
  openExternal(url) {
    if (!hasDom()) return Promise.reject(unsupported('shell.openExternal()'));
    window.open(String(url), '_blank', 'noopener');
    return Promise.resolve();
  },
  // No W3C API opens local paths; Electron reports failure as a message.
  openPath() { return Promise.resolve('shell.openPath() is not supported by this w3cos runtime'); },
  showItemInFolder() {},
  trashItem() { return Promise.reject(unsupported('shell.trashItem()')); },
  beep() {},
  writeShortcutLink() { return false; },
  readShortcutLink() { throw unsupported('shell.readShortcutLink()'); },
};

// --- Menu (model only; no native menu bar yet) ---------------------------------

let applicationMenu = null;

class MenuItem {
  constructor(options) {
    Object.assign(this, options || {});
    this.enabled = this.enabled !== false;
    this.visible = this.visible !== false;
    if (this.submenu && !(this.submenu instanceof Menu)) this.submenu = Menu.buildFromTemplate(this.submenu);
  }
}

class Menu extends EventEmitter {
  constructor() {
    super();
    this.items = [];
  }
  static buildFromTemplate(template) {
    const menu = new Menu();
    for (const item of template || []) menu.append(item instanceof MenuItem ? item : new MenuItem(item));
    return menu;
  }
  static setApplicationMenu(menu) { applicationMenu = menu; }
  static getApplicationMenu() { return applicationMenu; }
  append(item) { this.items.push(item); }
  insert(position, item) { this.items.splice(position, 0, item); }
  getMenuItemById(id) {
    for (const item of this.items) {
      if (item.id === id) return item;
      const found = item.submenu && item.submenu.getMenuItemById(id);
      if (found) return found;
    }
    return null;
  }
  popup() { this.emit('menu-will-show'); }
  closePopup() { this.emit('menu-will-close'); }
}

// --- nativeTheme -----------------------------------------------------------------

const nativeTheme = new EventEmitter();
nativeTheme.themeSource = 'system';
Object.defineProperty(nativeTheme, 'shouldUseDarkColors', {
  get() {
    if (nativeTheme.themeSource !== 'system') return nativeTheme.themeSource === 'dark';
    return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches === true;
  },
});

module.exports = {
  app,
  BrowserWindow,
  WebContents,
  webContents: {
    getAllWebContents() { return windows.map((win) => win.webContents); },
    getFocusedWebContents() { return windows.length > 0 ? windows[0].webContents : null; },
    fromId(id) {
      const win = windows.find((candidate) => candidate.webContents.id === id);
      return win ? win.webContents : undefined;
    },
  },
  ipcMain,
  ipcRenderer,
  contextBridge,
  dialog,
  shell,
  Menu,
  MenuItem,
  nativeTheme,
};
