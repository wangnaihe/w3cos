// Electron platform services Code-OSS uses from the main process:
// the `vscode-file://` protocol, nativeTheme, screen and safeStorage.
const { app, BrowserWindow, protocol, net, nativeTheme, screen, safeStorage } = require('electron')
const path = require('node:path')
const { pathToFileURL } = require('node:url')

// Linux without a keyring: opt into the plain-text backend (w3cos always
// has its own local keyring).
app.commandLine.appendSwitch('password-store', 'basic')
protocol.registerSchemesAsPrivileged([
  { scheme: 'vscode-file', privileges: { standard: true, secure: true, supportFetchAPI: true, corsEnabled: true } }
])
let threw = false
try { screen.getPrimaryDisplay() } catch (e) { threw = true }
console.log('screen-before-ready-throws', threw)

app.whenReady().then(async () => {
  // nativeTheme
  console.log('theme', nativeTheme.themeSource, typeof nativeTheme.shouldUseDarkColors,
    nativeTheme.shouldUseHighContrastColors, nativeTheme.shouldUseInvertedColorScheme,
    nativeTheme.inForcedColorsMode, nativeTheme.prefersReducedTransparency)
  let updates = 0
  nativeTheme.on('updated', () => { updates++ })
  nativeTheme.themeSource = 'dark'
  console.log('dark', nativeTheme.themeSource, nativeTheme.shouldUseDarkColors)
  nativeTheme.themeSource = 'light'
  console.log('light', nativeTheme.themeSource, nativeTheme.shouldUseDarkColors)
  try { nativeTheme.themeSource = 'blue' } catch (e) { console.log('bad-source', e.constructor.name, JSON.stringify(e.message)) }
  console.log('after-bad', nativeTheme.themeSource)
  nativeTheme.themeSource = 'system'
  await new Promise((resolve) => setTimeout(resolve, 50))
  console.log('updates', updates)

  // screen
  const d = screen.getPrimaryDisplay()
  console.log('display-keys', Object.keys(d).sort().join(','))
  console.log('display', typeof d.id, typeof d.label, typeof d.scaleFactor, typeof d.bounds.width, d.bounds.x, d.bounds.y,
    d.size.width === d.bounds.width, d.workArea.width <= d.bounds.width, d.rotation, d.internal, d.touchSupport,
    d.monochrome, d.detected)
  const all = screen.getAllDisplays()
  console.log('all', Array.isArray(all), all.length >= 1, all[0].id === d.id)
  console.log('nearest', screen.getDisplayNearestPoint({ x: 1, y: 1 }).id === d.id,
    screen.getDisplayMatching({ x: 0, y: 0, width: 10, height: 10 }).id === d.id)
  const p = screen.getCursorScreenPoint()
  console.log('cursor', typeof p.x, typeof p.y, typeof screen.on, typeof screen.dipToScreenPoint)

  // safeStorage
  safeStorage.setUsePlainTextEncryption(true)
  console.log('ss', safeStorage.isEncryptionAvailable(), typeof safeStorage.getSelectedStorageBackend())
  const enc = safeStorage.encryptString('secret ✓ token')
  console.log('ss-enc', Buffer.isBuffer(enc), enc.toString('latin1').includes('secret'))
  console.log('ss-dec', safeStorage.decryptString(enc))
  console.log('ss-empty', JSON.stringify(safeStorage.decryptString(safeStorage.encryptString(''))))
  try { safeStorage.decryptString('nope') } catch (e) { console.log('ss-dec-type', e.constructor.name, JSON.stringify(e.message)) }
  try { safeStorage.decryptString(Buffer.from('garbage-bytes')) } catch (e) { console.log('ss-dec-bad', e.constructor.name, JSON.stringify(e.message)) }
  try { safeStorage.encryptString(5) } catch (e) { console.log('ss-enc-type', e.constructor.name, JSON.stringify(e.message)) }

  // protocol: Code-OSS serves its workbench from vscode-file://vscode-app/<path>
  console.log('handled-before', protocol.isProtocolHandled('vscode-file'))
  protocol.handle('vscode-file', (request) => {
    const url = new URL(request.url)
    console.log('handler', url.host, path.basename(url.pathname), request.method)
    if (url.host !== 'vscode-app') return new Response('denied', { status: 403 })
    return net.fetch(pathToFileURL(decodeURIComponent(url.pathname)).toString())
  })
  console.log('handled-after', protocol.isProtocolHandled('vscode-file'))
  const res = await net.fetch('vscode-file://vscode-app' + path.join(__dirname, 'data.json'))
  console.log('fetch', res.status, res.ok, JSON.stringify(await res.json()))
  const denied = await net.fetch('vscode-file://other/x')
  console.log('denied', denied.status, await denied.text())
  try { protocol.handle('vscode-file', () => null) } catch (e) { console.log('double-handle', e.message) }

  const win = new BrowserWindow({ show: false })
  await win.loadURL('vscode-file://vscode-app' + path.join(__dirname, 'page.html'))
  console.log('loaded', win.webContents.getURL().startsWith('vscode-file://vscode-app/'), win.getTitle())
  protocol.unhandle('vscode-file')
  console.log('handled-unhandled', protocol.isProtocolHandled('vscode-file'))
  win.close()
})
