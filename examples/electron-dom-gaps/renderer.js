// Probe the DOM / CSS surface Code-OSS and Monaco rely on; every line is
// `name => result` (or `name !! ErrorName: message`).
const lines = []
function check (name, fn) {
  let out
  try { out = '=> ' + JSON.stringify(fn()) } catch (e) { out = '!! ' + (e && e.name) + ': ' + (e && e.message) }
  lines.push(name + ' ' + out)
}
const $ = (s) => document.querySelector(s)
const cs = (el, p) => getComputedStyle(el).getPropertyValue(p)

// --- CSSOM: constructable + adopted stylesheets
check('adopted.ctor', () => {
  const sheet = new CSSStyleSheet()
  sheet.replaceSync('.adopted { color: rgb(255, 0, 0); } .adopted2 { margin-top: 4px; }')
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet]
  const el = document.createElement('div')
  el.className = 'adopted adopted2'
  document.body.appendChild(el)
  const r = [sheet.cssRules.length, cs(el, 'color'), cs(el, 'margin-top'), document.adoptedStyleSheets.length]
  sheet.insertRule('.adopted { color: rgb(0, 0, 255); }', 2)
  r.push(sheet.cssRules.length, cs(el, 'color'))
  sheet.deleteRule(2)
  r.push(cs(el, 'color'))
  document.adoptedStyleSheets = []
  r.push(cs(el, 'color'))
  el.remove()
  return r
})
check('adopted.replace', () => typeof new CSSStyleSheet().replace)
check('stylesheets', () => [document.styleSheets.length, document.styleSheets[0].cssRules.length > 3,
  document.styleSheets[0].cssRules[0].selectorText])
check('style.inject', () => {
  const style = document.createElement('style')
  style.textContent = '.injected { border-left-width: 7px; border-left-style: solid; }'
  document.head.appendChild(style)
  const el = document.createElement('div'); el.className = 'injected'; document.body.appendChild(el)
  const r = [cs(el, 'border-left-width'), document.styleSheets.length]
  style.remove()
  r.push(cs(el, 'border-left-width'))
  el.remove()
  return r
})

// --- computed style cascade
check('computed.var', () => [cs($('#themed'), 'color'), cs($('#themed'), 'padding-left'),
  getComputedStyle(document.documentElement).getPropertyValue('--fg').trim()])
check('computed.inherit', () => [cs($('#child'), 'color'), cs($('#child'), 'font-size')])
check('computed.nth', () => [...document.querySelectorAll('#list li')].map((li) => cs(li, 'font-weight') + '/' + cs(li, 'display')))
check('computed.setProperty', () => {
  const el = $('#themed')
  el.style.setProperty('--fg', 'rgb(4, 5, 6)')
  const r = [el.style.getPropertyValue('--fg'), cs(el, 'color')]
  el.style.setProperty('color', 'rgb(7, 7, 7)', 'important')
  r.push(el.style.getPropertyPriority('color'), cs(el, 'color'), el.style.cssText)
  el.style.cssText = ''
  r.push(cs(el, 'color'))
  return r
})

// --- selectors
check('selectors', () => [
  document.querySelectorAll('#list > li:not(.skip)').length,
  document.querySelectorAll('li:nth-child(2n+1)').length,
  document.querySelectorAll('[data-x^="ab"]').length,
  document.querySelectorAll(':is(#box, #dnd)').length,
  document.querySelectorAll('ul:has(> li.keep)').length,
  $('#child').closest('.inherit-parent') !== null,
  $('#box').matches('div.box:not(.x)'),
  document.querySelectorAll('li:first-child, li:last-of-type').length
])
check('innerHTML', () => $('#list').children[3].outerHTML + '|' + $('#child').parentElement.innerHTML)

// --- layout geometry
check('rect', () => {
  const r = $('#box').getBoundingClientRect()
  return [r.width, r.height, $('#box').offsetWidth, $('#box').clientWidth, $('#box').offsetHeight]
})

// --- scrolling
check('scroll.element', () => {
  const s = $('#scroller')
  const r = [s.scrollHeight, s.clientHeight]
  s.scrollTop = 30
  r.push(s.scrollTop)
  s.scrollTo({ top: 100, left: 20 })
  r.push(s.scrollTop, s.scrollLeft)
  s.scrollBy(0, 10)
  r.push(s.scrollTop)
  s.scrollTo(0, 99999)
  r.push(s.scrollTop === s.scrollHeight - s.clientHeight)
  s.scrollTop = -5
  r.push(s.scrollTop)
  return r
})
check('scroll.window', () => {
  window.scrollTo(0, 0); window.scrollTo({ top: 0, behavior: 'instant' }); window.scrollBy(0, 0)
  return [typeof window.scrollX, window.scrollY, typeof document.scrollingElement, typeof $('#box').scrollIntoView]
})

// --- drag and drop
check('dnd.datatransfer', () => {
  const dt = new DataTransfer()
  dt.setData('text/plain', 'hello')
  dt.setData('application/vnd.code.tree', '{"a":1}')
  const r = [dt.getData('text/plain'), dt.getData('text'), [...dt.types].join(','), dt.items.length,
    dt.dropEffect, dt.effectAllowed, dt.files.length]
  dt.clearData('text/plain')
  r.push([...dt.types].join(','))
  dt.effectAllowed = 'copyMove'; dt.dropEffect = 'copy'
  r.push(dt.effectAllowed, dt.dropEffect)
  return r
})
check('dnd.events', () => {
  const el = $('#dnd')
  const seen = []
  for (const type of ['dragstart', 'dragenter', 'dragover', 'drop', 'dragend']) {
    el.addEventListener(type, (e) => {
      seen.push(type + ':' + (e instanceof DragEvent) + ':' + (e instanceof MouseEvent) + ':' + (e.dataTransfer && e.dataTransfer.getData('text/plain')))
      if (type === 'dragover') e.preventDefault()
    })
  }
  const dt = new DataTransfer(); dt.setData('text/plain', 'x')
  for (const type of ['dragstart', 'dragenter', 'dragover', 'drop', 'dragend']) {
    const ok = el.dispatchEvent(new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt, clientX: 3 }))
    seen.push(ok)
  }
  return [el.draggable, document.createElement('div').draggable, document.createElement('img').draggable, seen.join(' ')]
})

// --- SVG / namespaces
check('svg', () => {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  const path = document.createElementNS('http://www.w3.org/2000/svg', 'path')
  path.setAttribute('d', 'M0 0L10 10')
  svg.appendChild(path)
  svg.setAttribute('viewBox', '0 0 16 16')
  return [svg.namespaceURI, svg.tagName, svg.localName, path.tagName, svg instanceof SVGElement, svg.getAttribute('viewBox'),
    document.createElement('div').namespaceURI]
})

// --- clipboard / commands
check('clipboard.event', () => {
  const e = new ClipboardEvent('copy', { clipboardData: new DataTransfer() })
  e.clipboardData.setData('text/plain', 'c')
  return [e.type, e instanceof Event, e.clipboardData.getData('text/plain'), typeof document.execCommand,
    document.queryCommandSupported('copy')]
})

// --- scheduling and misc globals
check('globals', () => [typeof requestIdleCallback, typeof cancelIdleCallback, typeof structuredClone,
  typeof queueMicrotask, typeof AbortSignal.timeout, typeof AbortSignal.any, btoa('hi'), atob('aGk='),
  structuredClone({ a: [1, new Map([[1, 2]])] }).a[1].get(1)])
check('css.ns', () => [CSS.supports('display', 'grid'), CSS.supports('(display: flex)'), CSS.supports('display', 'bogus'),
  CSS.escape('a.b#c'), CSS.escape('1x')])

// --- focus
check('focus', () => {
  const input = document.createElement('input')
  document.body.appendChild(input)
  $('#box').tabIndex = 0
  input.focus()
  // (A hidden window has no system focus: no focus events, no :focus.)
  const r = [document.activeElement === input, $('#box').tabIndex, document.createElement('div').tabIndex,
    input.tabIndex, $('#box').getAttribute('tabindex')]
  input.blur()
  r.push(document.activeElement === document.body)
  input.remove()
  return r
})

// --- dialogs / popover / inert / shadow DOM
check('dialog', () => {
  const d = document.createElement('dialog')
  document.body.appendChild(d)
  d.show(); const a = d.open
  d.close('x'); const r = [a, d.open, d.returnValue, typeof d.showModal]
  d.remove()
  return r
})
check('shadow', () => {
  const host = document.createElement('div')
  const root = host.attachShadow({ mode: 'open' })
  root.innerHTML = '<style>p{color:rgb(0, 128, 0)}</style><p>s</p>'
  document.body.appendChild(host)
  const r = [host.shadowRoot === root, root.querySelector('p').textContent, root.mode]
  host.remove()
  return r
})
check('misc', () => ['inert' in HTMLElement.prototype, typeof document.fonts,
  typeof document.elementFromPoint, typeof document.caretRangeFromPoint])

requestIdleCallback((deadline) => {
  lines.push('idle ' + typeof deadline.timeRemaining() + ' ' + typeof deadline.didTimeout)
  for (const line of lines) window.probe.line(line)
  window.probe.done()
}, { timeout: 200 })
