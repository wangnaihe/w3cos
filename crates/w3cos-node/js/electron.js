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
  // Chromium GPU feature map (`chrome://gpu`). w3cos composites natively, so
  // every feature reports `enabled`; callers such as Code-OSS's
  // GPUProcessMainService read `featureStatus.gpu_compositing`.
  getGPUFeatureStatus() {
    return {
      '2d_canvas': 'enabled',
      canvas_oop_rasterization: 'enabled',
      direct_rendering_display_compositor: 'enabled',
      gpu_compositing: 'enabled',
      multiple_raster_threads: 'enabled',
      opengl: 'enabled',
      rasterization: 'enabled',
      raw_draw: 'disabled_off',
      skia_graphite: 'disabled_off',
      video_decode: 'enabled',
      video_encode: 'enabled',
      vulkan: 'disabled_off',
      webgl: 'enabled',
      webgl2: 'enabled',
      webgpu: 'disabled_off',
    };
  }
  getGPUInfo() { return Promise.resolve({ auxAttributes: {}, gpuDevice: [] }); }
  getAppMetrics() { return []; }
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
  const report = (error) => {
    console.error(label === 'preload'
      ? 'Unable to load preload script: ' + file
      : 'Uncaught ' + (error && error.stack ? error.stack : error));
    if (label === 'preload') console.error(error);
  };
  try {
    // ES module scripts (`<script type="module">`, ESM preloads) are linked
    // as lazily evaluated modules: `load` returns their `import()` promise.
    const result = assets.load(key);
    if (result && typeof result.then === 'function') return result.then(undefined, report);
  } catch (error) {
    report(error);
  }
  return undefined;
}

// Returns a promise when an ES module script is still evaluating; classic
// pages finish synchronously as before.
function renderPage(contents, page) {
  document.head.innerHTML = page.head;
  document.body.innerHTML = page.body;
  document.title = page.title;
  const preload = contents._preferences.preload;
  let pending = preload ? runRendererScript(preload, 'preload') : undefined;
  for (const script of page.scripts) {
    pending = pending
      ? pending.then(() => runRendererScript(script, 'script'))
      : runRendererScript(script, 'script');
  }
  const contentLoaded = () => {
    document.dispatchEvent(new Event('DOMContentLoaded', { bubbles: true }));
  };
  if (pending) return pending.then(contentLoaded);
  contentLoaded();
  return undefined;
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
    this.session = session.defaultSession;
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
    if (protocolHandlers.has(schemeOf(href))) {
      // e.g. Code-OSS's `vscode-file://vscode-app/<path>/workbench.html`: the
      // handler decides which file backs the page.
      return protocolFile(href).then(
        (file) => this._load(href, file === null ? null : path.relative(assets.root, file)),
        () => this._load(href, null),
      );
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
        const finish = () => {
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
        };
        const rendering = renderPage(this, page);
        if (rendering) rendering.then(finish);
        else finish();
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

// --- webFrame / webUtils (renderer) -------------------------------------------

// One renderer with no page zoom: zoom calls are recorded so reads match.
let zoomLevel = 0;
const webFrame = {
  setZoomLevel(level) { zoomLevel = Number(level) || 0; },
  getZoomLevel() { return zoomLevel; },
  setZoomFactor(factor) { zoomLevel = Math.log(Number(factor) || 1) / Math.log(1.2); },
  getZoomFactor() { return Math.pow(1.2, zoomLevel); },
  setVisualZoomLevelLimits() { return Promise.resolve(); },
  getResourceUsage() { return {}; },
  clearCache() {},
};

const webUtils = {
  // `File` objects from the host's pickers carry their filesystem path.
  getPathForFile(file) { return file && typeof file.path === 'string' ? file.path : ''; },
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

// --- MessageChannelMain / utilityProcess -----------------------------------------
// Ports carry structured clones between main-process code; a utility process
// is an isolated worker thread (its own realm, `process.parentPort`) running a
// module the compiler linked in (`utilityProcess.fork(path.join(__dirname,
// 'child.js'))`). There is no second OS process: `pid` is this process's.

const workerThreads = require('worker_threads');
const structuredCodec = require('node:internal/w3cos_clone');

class MessagePortMain extends EventEmitter {
  constructor() {
    super();
    this._other = null;
    this._queue = [];
    this._started = false;
    this._closed = false;
    this._scheduled = false;
  }
  postMessage(message, transfer) {
    if (Array.isArray(transfer) && transfer.length > 0) throw unsupported('Transferring MessagePortMain objects');
    const data = structuredCodec.serialize(message);
    const other = this._other;
    if (this._closed || other === null || other._closed) return;
    other._queue.push(data);
    other._schedule();
  }
  _schedule() {
    if (!this._started || this._scheduled || this._queue.length === 0) return;
    this._scheduled = true;
    task(() => {
      this._scheduled = false;
      if (!this._started || this._closed || this._queue.length === 0) return;
      const data = structuredCodec.deserialize(this._queue.shift());
      this.emit('message', { data, ports: [] });
      this._schedule();
    });
  }
  start() {
    if (this._started || this._closed) return;
    this._started = true;
    this._schedule();
  }
  close() {
    if (this._closed) return;
    this._closed = true;
    const other = this._other;
    if (other !== null && !other._closed) {
      other._closed = true;
      task(() => other.emit('close'));
    }
  }
}

class MessageChannelMain {
  constructor() {
    this.port1 = new MessagePortMain();
    this.port2 = new MessagePortMain();
    this.port1._other = this.port2;
    this.port2._other = this.port1;
  }
}

const utilityChildren = new Set();

class UtilityProcess extends EventEmitter {
  constructor(modulePath, args, options) {
    super();
    const opts = options || {};
    this.pid = undefined;
    this.stdout = null;
    this.stderr = null;
    this._exited = false;
    this._worker = new workerThreads.Worker(modulePath, {
      workerData: { args: Array.isArray(args) ? args.map(String) : [], serviceName: opts.serviceName },
      name: opts.serviceName,
      w3cosKind: 'utility',
    });
    utilityChildren.add(this);
    this._worker.on('online', () => {
      this.pid = process.pid;
      this.emit('spawn');
    });
    this._worker.on('message', (message) => this.emit('message', message));
    this._worker.on('error', (error) => this.emit('error', 'FatalError', error && error.stack ? String(error.stack) : String(error)));
    this._worker.on('exit', (code) => {
      this._exited = true;
      this.pid = undefined;
      utilityChildren.delete(this);
      this.emit('exit', code);
    });
  }
  postMessage(message, transfer) {
    if (Array.isArray(transfer) && transfer.length > 0) throw unsupported('Transferring MessagePortMain objects');
    this._worker.postMessage(message);
  }
  kill() {
    if (this._exited) return false;
    this._worker.terminate();
    return true;
  }
}

const utilityProcess = {
  fork(modulePath, args, options) {
    return new UtilityProcess(String(modulePath), args, options);
  },
};

app.on('quit', () => {
  for (const child of Array.from(utilityChildren)) child.kill();
});

// --- protocol / net ---------------------------------------------------------------
//
// Code-OSS serves its workbench and resources from `vscode-file://vscode-app/
// <absolute path>` through a protocol handler. Handlers run in-process: a
// `protocol.handle()` handler gets a Request and returns a Response (usually
// `net.fetch()` of a file:// URL); `loadURL()` of a handled scheme shows the
// compiled page for the file the handler resolved to, and `net.fetch()` /
// renderer `fetch()` of a privileged scheme go through the handler too.

const fs = require('fs');
const { fileURLToPath, pathToFileURL } = require('url');

const BUILTIN_SCHEMES = ['http', 'https', 'file', 'ws', 'wss', 'data', 'blob', 'about', 'javascript'];
const protocolHandlers = new Map();
const fileInterceptors = new Map();
const privilegedSchemes = new Map();
// Responses produced by `net.fetch(file://...)`, to the file they read.
const responseFiles = new WeakMap();

function schemeOf(href) {
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(String(href));
  return match ? match[1].toLowerCase() : '';
}

function netError(code) {
  const error = new TypeError('net::' + code);
  error.code = code;
  return error;
}

function makeRequest(href, init) {
  const options = init || {};
  if (typeof Request === 'function') {
    try { return new Request(href, options); } catch (_) { /* fall back below */ }
  }
  return {
    url: href,
    method: String(options.method || 'GET').toUpperCase(),
    headers: options.headers || {},
    body: options.body === undefined ? null : options.body,
  };
}

function makeResponse(body, init) {
  if (typeof Response === 'function') return new Response(body, init);
  const bytes = body === null || body === undefined ? Buffer.alloc(0) : Buffer.from(body);
  const status = init && init.status !== undefined ? init.status : 200;
  return {
    status,
    ok: status >= 200 && status < 300,
    statusText: (init && init.statusText) || '',
    headers: (init && init.headers) || {},
    text() { return Promise.resolve(bytes.toString('utf8')); },
    json() { return Promise.resolve(JSON.parse(bytes.toString('utf8'))); },
    arrayBuffer() { return Promise.resolve(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.length)); },
  };
}

function fetchFile(href) {
  let file;
  try { file = fileURLToPath(href); } catch (_) { return Promise.reject(netError('ERR_INVALID_URL')); }
  return fs.promises.readFile(file).then(
    (data) => {
      const response = makeResponse(data, { status: 200 });
      responseFiles.set(response, file);
      return response;
    },
    () => Promise.reject(netError('ERR_FILE_NOT_FOUND')),
  );
}

// A legacy (`registerFileProtocol` / `interceptFileProtocol`) handler as a
// Request -> Response handler.
function fileProtocolHandler(handler) {
  return (request) => new Promise((resolve, reject) => {
    handler(request, (result) => {
      const target = typeof result === 'string' ? result : (result && result.path);
      if (typeof target !== 'string' || target === '') {
        reject(netError('ERR_FILE_NOT_FOUND'));
        return;
      }
      fetchFile(pathToFileURL(target).href).then(resolve, reject);
    });
  });
}

function runHandler(handler, href, init) {
  return Promise.resolve()
    .then(() => handler(makeRequest(href, init)))
    .then((response) => {
      if (response === null || response === undefined) throw netError('ERR_FAILED');
      return response;
    });
}

const net = {
  fetch(input, init) {
    const href = typeof input === 'string' ? input : String(input && input.url !== undefined ? input.url : input);
    const scheme = schemeOf(href);
    const bypass = init && init.bypassCustomProtocolHandlers;
    const handler = bypass
      ? undefined
      : protocolHandlers.get(scheme) || (scheme === 'file' ? fileInterceptors.get('file') : undefined);
    if (handler) return runHandler(handler, href, init);
    if (scheme === 'file') return fetchFile(href);
    if (typeof originalFetch === 'function') return originalFetch(href, init);
    return Promise.reject(netError('ERR_UNKNOWN_URL_SCHEME'));
  },
  isOnline() { return true; },
  get online() { return true; },
};

// Which file a URL of a handled scheme resolves to (null: not a file).
function protocolFile(href) {
  return net.fetch(href).then((response) => {
    const file = responseFiles.get(response);
    return file === undefined ? null : file;
  });
}

// Renderer `fetch()` of a privileged custom scheme goes through its handler.
const originalFetch = typeof fetch === 'function' ? fetch : undefined;
if (originalFetch !== undefined) {
  globalThis.fetch = function fetch(input, init) {
    const href = typeof input === 'string' ? input : String(input && input.url !== undefined ? input.url : input);
    const scheme = schemeOf(href);
    const privileges = privilegedSchemes.get(scheme);
    if (protocolHandlers.has(scheme) && privileges && privileges.supportFetchAPI) return net.fetch(href, init);
    return originalFetch(input, init);
  };
}

// `protocol.handle()` also takes the built-in schemes (it intercepts them,
// e.g. Code-OSS refuses `file:` loads); the legacy `register*Protocol` calls
// do not.
function registerHandler(map, scheme, handler, verb) {
  const name = String(scheme);
  const builtin = map === protocolHandlers && verb === 'boolean' && BUILTIN_SCHEMES.indexOf(name) >= 0;
  if (map.has(name) || builtin) {
    if (verb === 'boolean') return false;
    throw new Error('Failed to register protocol: ' + name);
  }
  map.set(name, handler);
  return verb === 'boolean' ? true : undefined;
}

const protocol = {
  registerSchemesAsPrivileged(schemes) {
    if (app._ready) {
      throw new Error('protocol.registerSchemesAsPrivileged should be called before app is ready');
    }
    for (const entry of schemes || []) {
      privilegedSchemes.set(String(entry.scheme), Object.assign({}, entry.privileges || {}));
    }
  },
  handle(scheme, handler) { registerHandler(protocolHandlers, scheme, handler, 'throw'); },
  unhandle(scheme) {
    if (!protocolHandlers.delete(String(scheme))) throw new Error('Failed to unregister protocol: ' + scheme);
  },
  isProtocolHandled(scheme) { return protocolHandlers.has(String(scheme)); },
  registerFileProtocol(scheme, handler) {
    return registerHandler(protocolHandlers, scheme, fileProtocolHandler(handler), 'boolean');
  },
  unregisterProtocol(scheme) { return protocolHandlers.delete(String(scheme)); },
  isProtocolRegistered(scheme) { return protocolHandlers.has(String(scheme)); },
  interceptFileProtocol(scheme, handler) {
    return registerHandler(fileInterceptors, scheme, fileProtocolHandler(handler), 'boolean');
  },
  uninterceptProtocol(scheme) { return fileInterceptors.delete(String(scheme)); },
  isProtocolIntercepted(scheme) { return fileInterceptors.has(String(scheme)); },
};

const session = {
  defaultSession: {
    protocol,
    fetch: net.fetch,
    clearCache() { return Promise.resolve(); },
    clearStorageData() { return Promise.resolve(); },
    setPermissionRequestHandler() {},
    setPermissionCheckHandler() {},
    setProxy() { return Promise.resolve(); },
    resolveProxy() { return Promise.resolve('DIRECT'); },
    getUserAgent() { return typeof navigator === 'object' && navigator !== null ? String(navigator.userAgent) : ''; },
    setUserAgent() {},
    webRequest: { onBeforeRequest() {}, onBeforeSendHeaders() {}, onHeadersReceived() {} },
  },
  fromPartition() { return session.defaultSession; },
};

// --- nativeTheme ------------------------------------------------------------------

const THEME_SOURCES = ['system', 'light', 'dark'];

function conversionFailure(value) {
  return new TypeError('Error processing argument at index 0, conversion failure from ' +
    (typeof value === 'string' ? value : ''));
}

function systemPrefersDark() {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches === true;
}

class NativeTheme extends EventEmitter {
  constructor() {
    super();
    this._source = 'system';
    this._updatePending = false;
    if (typeof matchMedia === 'function') {
      const query = matchMedia('(prefers-color-scheme: dark)');
      if (query && typeof query.addEventListener === 'function') {
        query.addEventListener('change', () => { if (this._source === 'system') this._scheduleUpdate(); });
      }
    }
  }
  get themeSource() { return this._source; }
  set themeSource(value) {
    if (typeof value !== 'string' || THEME_SOURCES.indexOf(value) < 0) throw conversionFailure(value);
    if (value === this._source) return;
    this._source = value;
    this._scheduleUpdate();
  }
  get shouldUseDarkColors() {
    return this._source === 'system' ? systemPrefersDark() : this._source === 'dark';
  }
  get shouldUseDarkColorsForSystemIntegratedUI() { return this.shouldUseDarkColors; }
  get shouldUseHighContrastColors() { return false; }
  get shouldUseInvertedColorScheme() { return false; }
  get inForcedColorsMode() { return false; }
  get prefersReducedTransparency() { return false; }
  // Changes within one turn produce one `updated`, like Electron.
  _scheduleUpdate() {
    if (this._updatePending) return;
    this._updatePending = true;
    setTimeout(() => {
      this._updatePending = false;
      this.emit('updated');
    }, 0);
  }
}

const nativeTheme = new NativeTheme();

// --- screen -----------------------------------------------------------------------
//
// One display, the surface the window system gives this app (`window.screen`).

function ensureReadyFor(moduleName) {
  if (!app._ready) throw new Error("The '" + moduleName + "' module can't be used before the app 'ready' event");
}

function primaryDisplay() {
  const s = typeof globalThis.screen === 'object' && globalThis.screen !== null ? globalThis.screen : {};
  const width = Number(s.width) > 0 ? Number(s.width) : 1920;
  const height = Number(s.height) > 0 ? Number(s.height) : 1080;
  const availWidth = Number(s.availWidth) > 0 ? Number(s.availWidth) : width;
  const availHeight = Number(s.availHeight) > 0 ? Number(s.availHeight) : height;
  const colorDepth = Number(s.colorDepth) > 0 ? Number(s.colorDepth) : 24;
  const scaleFactor = typeof devicePixelRatio === 'number' && devicePixelRatio > 0 ? devicePixelRatio : 1;
  return {
    accelerometerSupport: 'unknown',
    bounds: { x: 0, y: 0, width, height },
    colorDepth,
    colorSpace: '{primaries:BT709, transfer:SRGB, matrix:RGB, range:FULL}',
    depthPerComponent: colorDepth >= 30 ? 10 : 8,
    detected: true,
    displayFrequency: 60,
    id: 1,
    internal: false,
    label: '',
    maximumCursorSize: { width: 0, height: 0 },
    monochrome: false,
    nativeOrigin: { x: 0, y: 0 },
    rotation: 0,
    scaleFactor,
    size: { width, height },
    touchSupport: 'unknown',
    workArea: { x: 0, y: 0, width: availWidth, height: availHeight },
    workAreaSize: { width: availWidth, height: availHeight },
  };
}

let cursorPoint = { x: 0, y: 0 };
if (typeof addEventListener === 'function') {
  addEventListener('mousemove', (event) => {
    if (event && typeof event.screenX === 'number') cursorPoint = { x: event.screenX, y: event.screenY };
  });
}

function scalePoint(point, factor) {
  return { x: Math.round(point.x * factor), y: Math.round(point.y * factor) };
}

function scaleRect(rect, factor) {
  return {
    x: Math.round(rect.x * factor),
    y: Math.round(rect.y * factor),
    width: Math.round(rect.width * factor),
    height: Math.round(rect.height * factor),
  };
}

class Screen extends EventEmitter {
  getPrimaryDisplay() { ensureReadyFor('screen'); return primaryDisplay(); }
  getAllDisplays() { ensureReadyFor('screen'); return [primaryDisplay()]; }
  getDisplayNearestPoint() { return this.getPrimaryDisplay(); }
  getDisplayMatching() { return this.getPrimaryDisplay(); }
  getCursorScreenPoint() { ensureReadyFor('screen'); return Object.assign({}, cursorPoint); }
  dipToScreenPoint(point) { return scalePoint(point, primaryDisplay().scaleFactor); }
  screenToDipPoint(point) { return scalePoint(point, 1 / primaryDisplay().scaleFactor); }
  dipToScreenRect(_win, rect) { return scaleRect(rect, primaryDisplay().scaleFactor); }
  screenToDipRect(_win, rect) { return scaleRect(rect, 1 / primaryDisplay().scaleFactor); }
}

const screen = new Screen();

// --- powerMonitor / powerSaveBlocker / systemPreferences ----------------------------
//
// No OS power or preference events are delivered yet; the objects report a
// steady, mains-powered, active machine so Code-OSS main can subscribe.

class PowerMonitor extends EventEmitter {
  get onBatteryPower() { return false; }
  isOnBatteryPower() { return false; }
  getSystemIdleState(idleThreshold) { return 'active'; }
  getSystemIdleTime() { return 0; }
  getCurrentThermalState() { return 'unknown'; }
}
const powerMonitor = new PowerMonitor();

const powerSaveBlockers = new Set();
let nextPowerSaveBlockerId = 0;
const powerSaveBlocker = {
  start(type) {
    if (type !== 'prevent-app-suspension' && type !== 'prevent-display-sleep') {
      throw new TypeError('Invalid power save blocker type: ' + type);
    }
    const id = nextPowerSaveBlockerId++;
    powerSaveBlockers.add(id);
    return id;
  },
  stop(id) { return powerSaveBlockers.delete(id); },
  isStarted(id) { return powerSaveBlockers.has(id); },
};

class SystemPreferences extends EventEmitter {
  getAccentColor() { return '0078d7ff'; }
  getColor(color) { return '#000000'; }
  getAnimationSettings() { return { shouldRenderRichAnimations: true, scrollAnimationsEnabledBySystem: true, prefersReducedMotion: false }; }
  getMediaAccessStatus(mediaType) { return 'granted'; }
  askForMediaAccess(mediaType) { return Promise.resolve(true); }
  getUserDefault() { return undefined; }
  isAeroGlassEnabled() { return false; }
  canPromptTouchID() { return false; }
}
const systemPreferences = new SystemPreferences();

// --- globalShortcut ---------------------------------------------------------------------
//
// Accelerators are tracked but never fire: there is no OS-wide hook.

const globalShortcuts = new Map();
const globalShortcut = {
  register(accelerator, callback) {
    if (globalShortcuts.has(accelerator)) return false;
    globalShortcuts.set(accelerator, callback);
    return true;
  },
  registerAll(accelerators, callback) { for (const a of accelerators) globalShortcut.register(a, callback); },
  isRegistered(accelerator) { return globalShortcuts.has(accelerator); },
  unregister(accelerator) { globalShortcuts.delete(accelerator); },
  unregisterAll() { globalShortcuts.clear(); },
};

// --- clipboard ----------------------------------------------------------------------------
//
// Process-local store (main and renderer share the process); not yet the OS
// clipboard.

const clipboardData = new Map();
function clipboardKey(type) { return type === 'selection' ? 'selection' : 'clipboard'; }
function clipboardSlot(type) {
  const key = clipboardKey(type);
  if (!clipboardData.has(key)) clipboardData.set(key, {});
  return clipboardData.get(key);
}
const clipboard = {
  readText(type) { return clipboardSlot(type).text || ''; },
  writeText(text, type) { clipboardData.set(clipboardKey(type), { text: String(text) }); },
  readHTML(type) { return clipboardSlot(type).html || ''; },
  writeHTML(markup, type) { clipboardData.set(clipboardKey(type), { html: String(markup) }); },
  readRTF(type) { return clipboardSlot(type).rtf || ''; },
  writeRTF(text, type) { clipboardData.set(clipboardKey(type), { rtf: String(text) }); },
  readBookmark() { return { title: '', url: '' }; },
  readFindText() { return clipboardSlot('find').find || ''; },
  writeFindText(text) { clipboardSlot('find').find = String(text); },
  readImage() { return nativeImage.createEmpty(); },
  writeImage() {},
  readBuffer(format) { return Buffer.from(clipboardSlot().buffers?.[format] || []); },
  writeBuffer(format, buffer) { const slot = clipboardSlot(); slot.buffers = { ...(slot.buffers || {}), [format]: Buffer.from(buffer) }; },
  write(data, type) {
    const slot = {};
    if (data.text !== undefined) slot.text = String(data.text);
    if (data.html !== undefined) slot.html = String(data.html);
    if (data.rtf !== undefined) slot.rtf = String(data.rtf);
    clipboardData.set(clipboardKey(type), slot);
  },
  availableFormats(type) {
    const slot = clipboardSlot(type);
    const formats = [];
    if (slot.text !== undefined) formats.push('text/plain');
    if (slot.html !== undefined) formats.push('text/html');
    if (slot.rtf !== undefined) formats.push('text/rtf');
    return formats;
  },
  has(format, type) { return clipboard.availableFormats(type).indexOf(format) >= 0; },
  clear(type) { clipboardData.delete(clipboardKey(type)); },
};

// --- nativeImage ------------------------------------------------------------------------
//
// Images keep their encoded bytes; decoding and resizing are not provided.

class NativeImage {
  constructor(buffer) { this._buffer = buffer ? Buffer.from(buffer) : Buffer.alloc(0); }
  isEmpty() { return this._buffer.length === 0; }
  getSize() { return { width: 0, height: 0 }; }
  toPNG() { return Buffer.from(this._buffer); }
  toJPEG() { return Buffer.from(this._buffer); }
  toBitmap() { return Buffer.alloc(0); }
  getBitmap() { return Buffer.alloc(0); }
  toDataURL() { return 'data:image/png;base64,' + this._buffer.toString('base64'); }
  resize() { return this; }
  crop() { return this; }
  setTemplateImage() {}
  isTemplateImage() { return false; }
  getAspectRatio() { return 1; }
  addRepresentation() {}
}
const nativeImage = {
  createEmpty() { return new NativeImage(); },
  createFromBuffer(buffer) { return new NativeImage(buffer); },
  createFromPath(file) {
    try { return new NativeImage(require('fs').readFileSync(String(file))); } catch (e) { return new NativeImage(); }
  },
  createFromDataURL(url) {
    const match = /^data:[^;,]*(;base64)?,(.*)$/.exec(String(url));
    if (!match) return new NativeImage();
    return new NativeImage(match[1] ? Buffer.from(match[2], 'base64') : Buffer.from(decodeURIComponent(match[2])));
  },
  createFromNamedImage() { return new NativeImage(); },
};

// --- Notification / contentTracing / desktopCapturer ----------------------------------

class Notification extends EventEmitter {
  static isSupported() { return false; }
  constructor(options) {
    super();
    Object.assign(this, { title: '', body: '', silent: false }, options || {});
  }
  show() {}
  close() { this.emit('close', { sender: this }); }
}

const contentTracing = {
  getCategories() { return Promise.resolve([]); },
  startRecording() { return Promise.resolve(); },
  stopRecording(resultFilePath) { return Promise.resolve(resultFilePath || ''); },
  getTraceBufferUsage() { return Promise.resolve({ value: 0, percentage: 0 }); },
};

const desktopCapturer = {
  getSources() { return Promise.resolve([]); },
};

// --- safeStorage ------------------------------------------------------------------
//
// Sealed with the runtime's local keyring (AES-256-GCM, see
// `w3cos-runtime/src/node_safe_storage.rs`) instead of the OS keychain.

const nodeHost = typeof __w3cos_node_host === 'object' ? __w3cos_node_host : null;
const DECRYPT_ERRORS = {
  notEncrypted: 'Ciphertext does not appear to be encrypted.',
  failed: 'Decryption failed.',
  unavailable: 'Decryption is not available.',
};

function keyringAvailable() {
  return nodeHost !== null && typeof nodeHost.safeStorageEncrypt === 'function';
}

const safeStorage = {
  isEncryptionAvailable() { return keyringAvailable(); },
  isAsyncEncryptionAvailable() { return Promise.resolve(keyringAvailable()); },
  getSelectedStorageBackend() { return keyringAvailable() ? nodeHost.safeStorageBackend() : 'unknown'; },
  setUsePlainTextEncryption() {},
  encryptString(plainText) {
    if (typeof plainText !== 'string') throw conversionFailure(plainText);
    const result = keyringAvailable() ? nodeHost.safeStorageEncrypt(plainText) : { error: {} };
    if (result.error) {
      throw new Error('Error while encrypting the text provided to safeStorage.encryptString. Encryption is not available.');
    }
    return Buffer.from(result.blob);
  },
  decryptString(encrypted) {
    if (!Buffer.isBuffer(encrypted)) throw new Error('Expected the first argument of decryptString() to be a buffer');
    const result = keyringAvailable() ? nodeHost.safeStorageDecrypt(encrypted) : { error: { code: 'unavailable' } };
    if (result.error) {
      throw new Error('Error while decrypting the ciphertext provided to safeStorage.decryptString. ' +
        (DECRYPT_ERRORS[result.error.code] || DECRYPT_ERRORS.failed));
    }
    return result.text;
  },
  encryptStringAsync(plainText) {
    return new Promise((resolve) => resolve(safeStorage.encryptString(plainText)));
  },
  decryptStringAsync(encrypted) {
    return new Promise((resolve) => resolve({ shouldReEncrypt: false, result: safeStorage.decryptString(encrypted) }));
  },
};

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
  webFrame,
  webUtils,
  dialog,
  shell,
  Menu,
  MenuItem,
  nativeTheme,
  screen,
  safeStorage,
  protocol,
  net,
  session,
  MessageChannelMain,
  MessagePortMain,
  utilityProcess,
  powerMonitor,
  powerSaveBlocker,
  systemPreferences,
  globalShortcut,
  clipboard,
  nativeImage,
  Notification,
  contentTracing,
  desktopCapturer,
};
