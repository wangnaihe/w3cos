#!/usr/bin/env python3
"""Emit static HTML mockups for the w3cos shell UX spec."""

from pathlib import Path

OUT = Path(__file__).resolve().parent

ICONS = {
    "folder": '<path d="M4 8.2A2 2 0 0 1 6 6.2h3.1L11 8.2h7a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8.2Z"/>',
    "term": '<rect x="4" y="5.5" width="16" height="13" rx="2"/><path d="M7.5 9.5 10 12l-2.5 2.5M12 14.5h4.5"/>',
    "sliders": '<path d="M4 8h16M4 16h16"/><circle cx="9" cy="8" r="2.2"/><circle cx="15" cy="16" r="2.2"/>',
    "compass": '<circle cx="12" cy="12" r="7.5"/><path d="m14.6 9.4-1.6 3.6-3.6 1.6 1.6-3.6 3.6-1.6Z"/>',
    "pen": '<path d="M4.5 19.5h4L19 9a2.1 2.1 0 0 0-3-3L5.5 16.5v3Z"/><path d="M13.5 7.5l3 3"/>',
    "grid": '<rect x="4" y="4" width="6.5" height="6.5" rx="1.4"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1.4"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1.4"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.4"/>',
    "lock": '<rect x="6" y="10.5" width="12" height="9" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>',
    "bell": '<path d="M6 16.5h12l-1.2-2V10.2a4.8 4.8 0 0 0-9.6 0v4.3L6 16.5Z"/><path d="M10 16.5a2 2 0 0 0 4 0"/>',
    "devices": '<rect x="3" y="6" width="12" height="8" rx="1.4"/><path d="M7 17.5h6M10 14v3.5"/><rect x="16" y="8" width="5" height="9" rx="1.2"/>',
    "close": '<path d="M7 7l10 10M17 7 7 17"/>',
    "min": '<path d="M6 12h12"/>',
    "max": '<rect x="6" y="6" width="12" height="12" rx="1.5"/>',
    "search": '<circle cx="11" cy="11" r="5.5"/><path d="m15.5 15.5 3.5 3.5"/>',
    "enter": '<path d="M19 7v5.2A2.8 2.8 0 0 1 16.2 15H6"/><path d="M9.5 11.5 6 15l3.5 3.5"/>',
    "check": '<path d="m5 12.5 4.2 4.2L19 7.2"/>',
    "file": '<path d="M7 3.5h6.5L18 8v12.5H7V3.5Z"/><path d="M13.5 3.5V8H18"/>',
    "image": '<rect x="4" y="5" width="16" height="14" rx="2"/><circle cx="9" cy="10" r="1.4"/><path d="m4 16 4.5-3.5L12 16l2.2-2 5.8 4.2"/>',
    "phone": '<rect x="8" y="3" width="8" height="18" rx="2"/><path d="M11 18.5h2"/>',
    "desktop": '<rect x="3.5" y="5" width="17" height="11" rx="1.6"/><path d="M8 19.5h8M12 16v3.5"/>',
    "shield": '<path d="M12 3.5 19 6.5v5.2c0 4.2-2.8 6.8-7 8.8-4.2-2-7-4.6-7-8.8V6.5L12 3.5Z"/>',
    "receipt": '<path d="M7 3.5h10V20l-2-1.3L13 20l-2-1.3L9 20l-2-1.3V3.5Z"/><path d="M9.5 8h5M9.5 11.5h5"/>',
    "stop": '<rect x="7" y="7" width="10" height="10" rx="1.5"/>',
    "link": '<path d="M9 12H7.5a3.5 3.5 0 0 1 0-7H10"/><path d="M15 12h1.5a3.5 3.5 0 0 0 0-7H14"/><path d="M8 12h8"/>',
    "warn": '<path d="M12 4.5 20.5 19H3.5L12 4.5Z"/><path d="M12 10v4.5M12 17h.01"/>',
    "clip": '<rect x="7" y="5" width="10" height="15" rx="1.6"/><path d="M9 5.2V4.2A1.2 1.2 0 0 1 10.2 3h3.6A1.2 1.2 0 0 1 15 4.2v1"/>',
}


def ico(name: str, size: int = 24) -> str:
    return (
        f'<svg class="ico" width="{size}" height="{size}" viewBox="0 0 24 24" '
        f'fill="none" stroke="currentColor" stroke-width="1.7" '
        f'stroke-linecap="round" stroke-linejoin="round">{ICONS[name]}</svg>'
    )


def win_btns() -> str:
    return (
        '<div class="win-btns">'
        f'<button class="win-btn" aria-label="最小化">{ico("min", 14)}</button>'
        f'<button class="win-btn" aria-label="最大化">{ico("max", 14)}</button>'
        f'<button class="win-btn close" aria-label="关闭">{ico("close", 14)}</button>'
        "</div>"
    )


def titlebar(title: str, mark: str = "") -> str:
    cls = f' class="mark {mark}"' if mark else ' class="mark"'
    return (
        f'<div class="titlebar"><i{cls}></i><h2>{title}</h2>{win_btns()}</div>'
    )


def dock(running: str = "", notes: int = 0, l2: str = "L2 关") -> str:
    files_on = " on" if running in {"files", "both"} else ""
    term_on = " on" if running == "term" else ""
    apps = ""
    if running in {"files", "both"}:
        apps += f'<div class="running{files_on}" aria-label="文件">{ico("folder", 20)}</div>'
    if running in {"term", "both"}:
        apps += f'<div class="running term{term_on}" aria-label="终端">{ico("term", 20)}</div>'
    badge = f'<i class="badge">{notes}</i>' if notes else ""
    return f'''
    <footer class="dock">
      <button class="dock-btn" aria-label="启动器">{ico("grid", 20)}</button>
      {apps}
      <div class="spacer"></div>
      <div class="tray">
        <button class="dock-btn" aria-label="已锁定">{ico("lock", 18)}</button>
        <button class="dock-btn" aria-label="设备">{ico("devices", 18)}</button>
        <button class="dock-btn" aria-label="通知">{ico("bell", 18)}{badge}</button>
        <span class="l2flag">{l2}</span>
        <span class="clock">14:32</span>
      </div>
    </footer>'''


def intent(value: str, placeholder: str, card: str = "", go_label: str = "提议") -> str:
    shown = value or ""
    ph = "" if value else f' placeholder="{placeholder}"'
    val = f' value="{shown}"' if shown else ""
    return f'''
    <div class="intent-wrap">
      {card}
      <div class="intent">
        <input aria-label="意图"{ph}{val} />
        <button class="go" aria-label="{go_label}">{ico("enter", 18)}</button>
      </div>
    </div>'''


def icons_desktop() -> str:
    items = [
        ("folder", "jade", "文件"),
        ("term", "", "终端"),
        ("sliders", "amber", "设置"),
        ("compass", "blue", "浏览器"),
        ("pen", "", "编辑器"),
    ]
    bits = []
    for name, tone, label in items:
        cls = f"squircle {tone}".strip()
        bits.append(
            f'<button class="dicon"><span class="{cls}">{ico(name, 28)}</span><span>{label}</span></button>'
        )
    return f'<div class="dicons">{"".join(bits)}</div>'


def files_body() -> str:
    rows = [
        ("folder", "w3cos", "文件夹", "", True),
        ("receipt", "收据", "文件夹", "", False),
        ("file", "README.md", "Markdown", "4 KB", False),
        ("image", "截图.png", "PNG", "245 KB", False),
    ]
    lines = []
    for icon, name, kind, size, sel in rows:
        cls = "file sel" if sel else "file"
        lines.append(
            f'<div class="{cls}">{ico(icon, 18)}<span>{name}</span><small>{kind}</small><small>{size}</small></div>'
        )
    side = "".join(
        f'<button class="{"active" if i == 0 else ""}">{n}</button>'
        for i, n in enumerate(["家", "文档", "下载", "图片"])
    )
    return f'<div class="body-row"><nav class="side">{side}</nav><div class="main">{"".join(lines)}</div></div>'


def page(title: str, width: int, height: int, body: str) -> str:
    return f'''<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <title>{title}</title>
  <link rel="stylesheet" href="system.css" />
  <style>html,body{{width:{width}px;height:{height}px;overflow:hidden;}}</style>
</head>
<body>
{body}
</body>
</html>
'''


def desktop_home() -> str:
    hint = '<p class="hint">打开文件、夜间模式，或一句完整的话</p>'
    card = hint
    body = f'''<div class="os">
      {icons_desktop()}
      {intent("", "想做什么", card)}
      {dock()}
    </div>'''
    return page("桌面", 1440, 900, body)


def desktop_confirm() -> str:
    card = '''
      <section class="card" role="dialog" aria-label="确认意图">
        <span class="tier">L0 规则</span>
        <h3>打开文件</h3>
        <p>切换到文件窗口。不读取、不修改磁盘上的内容。</p>
        <div class="actions">
          <button class="btn ghost">取消</button>
          <button class="btn primary">确认</button>
        </div>
        <p class="fine">确认后才会执行，并写下一张收据。按 Esc 取消。</p>
      </section>'''
    win = f'''<div class="window" style="left:390px;top:78px;width:760px;height:520px">
      {titlebar("文件")}
      {files_body()}
    </div>'''
    body = f'''<div class="os">
      <div class="stage">{win}</div>
      {intent("打开文件", "", card)}
      {dock("files")}
    </div>'''
    return page("确认意图", 1440, 900, body)


def desktop_permission() -> str:
    term = '''<pre class="term"><span class="dim">只允许 echo、pwd、help。没有系统 shell。</span>
<span class="prompt">lin@书桌:~$</span> echo hello</pre>'''
    win = f'''<div class="window" style="left:360px;top:96px;width:720px;height:460px">
      {titlebar("终端", "amber")}
      {term}
    </div>'''
    sheet = f'''
    <div class="scrim"></div>
    <section class="sheet" role="dialog" aria-label="权限">
      <span class="tier">需要额外能力</span>
      <h3>允许运行这一次 echo</h3>
      <p>这不是意图确认。确认「打开终端」已经完成。现在要单独授权进程能力，用完即收回。</p>
      <div class="cap">
        <div><b>进程</b><br />只放行 echo、pwd、help</div>
        <span>w3cos-process<br/>一次</span>
      </div>
      <div class="actions">
        <button class="btn danger">拒绝</button>
        <button class="btn primary">仅此一次</button>
      </div>
      <p class="fine">不提供「始终允许」。拒绝也会写收据。身份必须已解锁。</p>
    </section>'''
    body = f'''<div class="os">
      <div class="stage">{win}{sheet}</div>
      {intent("echo hello", "")}
      {dock("term")}
    </div>'''
    return page("权限", 1440, 900, body)


def desktop_receipts() -> str:
    rows = [
        ("14:32", "打开文件", "L0 · 已切换到文件", "ok", "允许"),
        ("14:28", "夜间模式", "L1 · 你取消了，没有执行", "stop", "取消"),
        ("14:11", "运行 echo", "L0 · 当时还锁着", "no", "拒绝"),
        ("13:58", "把安装步骤总结一下", "未识别 · 没有对应命令", "unk", "未识别"),
    ]
    items = []
    for time, title, sub, kind, label in rows:
        items.append(
            f'<article class="receipt"><time>{time}</time><div><b>{title}</b><br /><em>{sub}</em></div><span class="pill {kind}">{label}</span></article>'
        )
    drawer = f'''
    <aside class="drawer" aria-label="收据">
      <header><h3 style="margin:0">收据</h3><button class="btn ghost">关闭</button></header>
      <p class="who">did:key:z6MkhaXg…ta2doK</p>
      <div class="filters">
        <button class="chip on">全部</button>
        <button class="chip">允许</button>
        <button class="chip">拒绝</button>
        <button class="chip">取消</button>
      </div>
      {''.join(items)}
      <p class="fine">只追加。换钥匙不会改写已经写下的收据。</p>
    </aside>'''
    body = f'''<div class="os">
      {icons_desktop()}
      {drawer}
      {intent("", "想做什么", '<p class="hint">打开文件、夜间模式，或一句完整的话</p>')}
      {dock()}
    </div>'''
    return page("收据", 1440, 900, body)


def desktop_lock() -> str:
    card = f'''
    <section class="lock-card">
      <div class="lock-seal">{ico("lock", 28)}</div>
      <h1>已锁定</h1>
      <p class="who">did:key:z6MkhaXg…ta2doK</p>
      <p>没有账号，也不走链。钥匙留在这台设备上。</p>
      <div style="margin-top:18px">
        <button class="btn primary block">用本机通行证解锁</button>
      </div>
      <p class="fine">解锁只影响这台设备。别的设备上的确认不会被一起放开。</p>
    </section>'''
    body = f'<div class="os">{card}</div>'
    return page("锁定", 1440, 900, body)


def desktop_launcher() -> str:
    apps = [
        ("folder", "jade", "文件"),
        ("term", "", "终端"),
        ("sliders", "amber", "设置"),
        ("compass", "blue", "浏览器"),
        ("pen", "", "编辑器"),
        ("receipt", "", "收据"),
        ("devices", "", "设备"),
        ("bell", "", "通知"),
    ]
    cells = []
    for name, tone, label in apps:
        cls = f"squircle {tone}".strip()
        cells.append(f'<button><span class="{cls}" style="width:48px;height:48px;border-radius:14px">{ico(name, 22)}</span>{label}</button>')
    shared = [
        ("desktop", "共享 Hello"),
        ("compass", "页面"),
        ("grid", "计数器"),
    ]
    shared_cells = []
    for name, label in shared:
        shared_cells.append(
            f'<button><span class="squircle" style="width:48px;height:48px;border-radius:14px">{ico(name, 22)}</span>{label}</button>'
        )
    panel = f'''
    <section class="launch" aria-label="启动器">
      <div class="search">{ico("search", 18)} 查找应用</div>
      <div class="sec">本机</div>
      <div class="agrid">{''.join(cells)}</div>
      <div class="sec">共享运行时</div>
      <div class="agrid">{''.join(shared_cells)}</div>
    </section>'''
    body = f'''<div class="os">
      {icons_desktop()}
      <div class="scrim" style="inset:0"></div>
      {panel}
      {dock()}
    </div>'''
    return page("启动器", 1440, 900, body)


def desktop_notifications() -> str:
    notes = f'''
    <section class="notes" aria-label="通知">
      <header><h3 style="margin:0">通知</h3><button class="chip">不打扰</button></header>
      <article class="note">
        <div class="bubble">{ico("check", 18)}</div>
        <div><b>已打开文件</b><span>收据 #128 · L0 · 刚刚</span></div>
      </article>
      <article class="note">
        <div class="bubble warn">{ico("warn", 18)}</div>
        <div><b>没有运行 echo</b><span>当时还锁着。收据 #127 已记下拒绝。</span></div>
      </article>
      <article class="note">
        <div class="bubble">{ico("phone", 18)}</div>
        <div><b>手机在同一网络</b><span>尚未配对。打开设备可以核对短码。</span></div>
      </article>
    </section>'''
    body = f'''<div class="os">
      {icons_desktop()}
      {notes}
      {intent("", "想做什么")}
      {dock(notes=2)}
    </div>'''
    return page("通知", 1440, 900, body)


def desktop_windows() -> str:
    back = f'''<div class="window back" style="left:72px;top:56px;width:520px;height:420px">
      {titlebar("终端", "amber")}
      <pre class="term"><span class="prompt">lin@书桌:~$</span> pwd
/home/lin/w3cos</pre>
    </div>'''
    front = f'''<div class="window" style="left:210px;top:128px;width:560px;height:480px">
      {titlebar("文件")}
      {files_body()}
    </div>'''
    snap = '<div class="snap">松开放到右半边</div>'
    body = f'''<div class="os">
      <div class="stage">{back}{snap}{front}</div>
      {intent("", "想做什么")}
      {dock("both")}
    </div>'''
    return page("窗口", 1440, 900, body)


def state_empty() -> str:
    drawer = '''
    <aside class="drawer">
      <header><h3 style="margin:0">收据</h3><button class="btn ghost">关闭</button></header>
      <div class="empty">
        <b>还没有收据</b>
        你确认并执行之后，每一条会出现在这里。取消和拒绝也会留下。
      </div>
    </aside>'''
    body = f'''<div class="os">
      {icons_desktop()}
      {drawer}
      {intent("", "想做什么", '<p class="hint">还没有进行中的意图</p>')}
      {dock()}
    </div>'''
    return page("空状态", 1440, 900, body)


def state_loading() -> str:
    card = '''
      <section class="card" role="status">
        <span class="tier l2">L2 规划</span>
        <h3>正在本机规划</h3>
        <p>还没有执行。规划器是你打开的，默认并不运行。</p>
        <div class="step"><i class="dot"></i><div class="skel" style="width:72%"></div></div>
        <div class="step"><i class="dot" style="opacity:.4"></i><div class="skel" style="width:48%"></div></div>
        <div class="actions">
          <button class="btn danger">停止</button>
        </div>
      </section>'''
    body = f'''<div class="os">
      {icons_desktop()}
      {intent("把 README 里的安装步骤总结一下", "", card)}
      {dock(l2="L2 开")}
    </div>'''
    return page("规划中", 1440, 900, body)


def state_error() -> str:
    card = '''
      <section class="card" role="alert">
        <span class="tier">未识别</span>
        <h3>这句话没有对应的动作</h3>
        <p>「帮我把系统重装一遍」不会执行。已经记了一张未识别收据。</p>
        <div class="actions">
          <button class="btn ghost">看我能做什么</button>
          <button class="btn primary">改一句</button>
        </div>
      </section>'''
    body = f'''<div class="os">
      {icons_desktop()}
      {intent("帮我把系统重装一遍", "", card)}
      {dock()}
    </div>'''
    return page("错误", 1440, 900, body)


def phone_status(time="9:41") -> str:
    return f'''<div class="status"><span>{time}</span><span class="right">{ico("lock", 14)} <span style="font-variant-numeric:tabular-nums">100%</span></span></div>'''


def phone_home() -> str:
    items = [
        ("folder", "jade", "文件"),
        ("term", "", "终端"),
        ("sliders", "amber", "设置"),
        ("compass", "blue", "浏览器"),
        ("pen", "", "编辑器"),
        ("devices", "", "设备"),
    ]
    cells = []
    for name, tone, label in items:
        cls = f"squircle {tone}".strip()
        cells.append(f'<button class="dicon"><span class="{cls}">{ico(name, 26)}</span><span>{label}</span></button>')
    body = f'''<div class="phone">
      {phone_status()}
      <div class="phone-icons">{''.join(cells)}</div>
      <div class="bottom-intent">
        <p class="hint">打开文件，或一句完整的话</p>
        <div class="intent"><input aria-label="意图" placeholder="想做什么" /><button class="go" aria-label="提议">{ico("enter", 18)}</button></div>
      </div>
      <div class="home-ind"></div>
    </div>'''
    return page("手机主屏", 390, 844, body)


def phone_confirm() -> str:
    body = f'''<div class="phone">
      {phone_status()}
      <div style="padding:8px 12px 0">
        <div class="window inplace">
          {titlebar("文件")}
          <div class="main">
            <div class="file sel">{ico("folder", 18)}<span>w3cos</span><small>文件夹</small><small></small></div>
            <div class="file">{ico("file", 18)}<span>README.md</span><small>4 KB</small><small></small></div>
          </div>
        </div>
      </div>
      <section class="sheet-m" role="dialog" aria-label="确认">
        <div class="grab"></div>
        <span class="tier">L0 规则</span>
        <h3 style="margin:8px 0 4px;font-size:20px">打开文件</h3>
        <p>会离开当前页面，打开文件。不改动磁盘。</p>
        <div class="actions">
          <button class="btn ghost">取消</button>
          <button class="btn primary">确认</button>
        </div>
      </section>
    </div>'''
    return page("手机确认", 390, 844, body)


def phone_switcher() -> str:
    body = f'''<div class="phone" style="background:#0e141b">
      {phone_status()}
      <div class="switcher">
        <article class="scard">
          <header><i class="mark"></i> 文件</header>
          <div class="fill">w3cos / README.md</div>
        </article>
        <article class="scard dim">
          <header><i class="mark amber"></i> 终端</header>
          <div class="fill">lin@书桌:~$</div>
        </article>
      </div>
      <p class="hint" style="position:absolute;left:0;right:0;bottom:92px">上滑一张卡片即可关闭。位置不带到别的设备。</p>
      <div class="bottom-intent">
        <div class="intent"><input aria-label="意图" placeholder="想做什么" /><button class="go" aria-label="提议">{ico("enter", 18)}</button></div>
      </div>
      <div class="home-ind"></div>
    </div>'''
    return page("手机切换", 390, 844, body)


def phone_notes() -> str:
    body = f'''<div class="phone">
      {phone_status()}
      <div class="phone-icons" style="opacity:.35">
        <button class="dicon"><span class="squircle jade">{ico("folder", 26)}</span><span>文件</span></button>
      </div>
      <section class="shade" aria-label="通知">
        <div class="grab"></div>
        <article class="note">
          <div class="bubble">{ico("check", 18)}</div>
          <div><b>已打开文件</b><span>收据 #128 · 点击查看</span></div>
        </article>
        <article class="note">
          <div class="bubble warn">{ico("warn", 18)}</div>
          <div><b>没有运行 echo</b><span>先解锁。拒绝已写入收据。</span></div>
        </article>
        <article class="note">
          <div class="bubble">{ico("desktop", 18)}</div>
          <div><b>书桌想把文件交过来</b><span>路径和草稿。确认不会自动发生。</span></div>
        </article>
      </section>
      <div class="home-ind"></div>
    </div>'''
    return page("手机通知", 390, 844, body)


def tablet_home() -> str:
    side = "".join(
        f'<button class="{"active" if i == 0 else ""}">{n}</button>'
        for i, n in enumerate(["家", "文档", "下载", "图片"])
    )
    rows = [
        ("folder", "w3cos", "文件夹", True),
        ("file", "README.md", "4 KB", False),
        ("image", "截图.png", "245 KB", False),
    ]
    lines = []
    for icon, name, meta, sel in rows:
        cls = "file sel" if sel else "file"
        lines.append(f'<div class="{cls}">{ico(icon, 18)}<span>{name}</span><small>{meta}</small><small></small></div>')
    rail_items = [("grid", False), ("folder", True), ("term", False), ("compass", False), ("sliders", False)]
    rail = []
    for name, on in rail_items:
        rail.append(f'<button class="dock-btn{" on" if on else ""}" aria-label="{name}">{ico(name, 22)}</button>')
    body = f'''<div class="tablet">
      <nav class="rail">{''.join(rail)}<div class="spacer"></div><span class="l2flag" style="border:0;writing-mode:horizontal-tb">L2 关</span></nav>
      <div style="position:relative;min-width:0">
        <div class="split">
          <div style="border-right:1px solid var(--line);padding:12px">
            <div class="side" style="width:auto;background:transparent;border:0;padding:0">{side}</div>
            <div style="margin-top:12px">{''.join(lines)}</div>
          </div>
          <div class="preview">
            <h2>README.md</h2>
            <p>安装：准备 Rust 1.85，然后在仓库根目录构建 shell。这一页是预览，不是第二套编辑器。</p>
            <p class="fine">平板宽度够两栏。再窄就回到手机的单栏栈。</p>
          </div>
        </div>
        <div class="bottom-intent">
          <div class="intent"><input aria-label="意图" placeholder="想做什么" /><button class="go" aria-label="提议">{ico("enter", 18)}</button></div>
        </div>
      </div>
    </div>'''
    return page("平板", 1194, 834, body)


def handoff_pair() -> str:
    phone = f'''
    <div class="device" aria-hidden="false">
      <div class="screen">
        <div class="status" style="padding:8px 4px 0"><span>9:41</span><span>手机</span></div>
        <div style="margin-top:36px;text-align:center">
          <div class="lock-seal" style="background:rgba(20,150,106,.16);color:#d9ffe9;border-color:rgba(20,150,106,.3)">{ico("link", 26)}</div>
          <h3 style="margin:14px 0 0;font-size:18px">配对书桌</h3>
          <p>核对数字是否一致</p>
          <div class="code">481 926</div>
          <p class="fine">90 秒后失效。不一致就停，不要猜。</p>
        </div>
      </div>
    </div>'''
    panel = f'''
    <section class="window" style="position:relative;width:640px;height:640px">
      {titlebar("设备")}
      <div style="padding:18px 18px 0">
        <h3 style="margin:0 0 4px;font-size:20px">同一把身份下的设备</h3>
        <p class="who">did:key:z6MkhaXg…ta2doK</p>
        <div class="list-dev">
          <div class="dev">{ico("desktop", 22)}<div><b>书桌</b><span>本机 · 已解锁这台</span></div><span class="pill ok">本机</span></div>
          <div class="dev" style="outline:1px solid rgba(20,150,106,.5)">{ico("phone", 22)}<div><b>手机</b><span>局域网发现 · 尚未信任</span></div><span class="pill stop">待核对</span></div>
        </div>
        <p style="margin:16px 0 0">两边都要看到同一个短码。没有账号服务器。</p>
        <div class="code">481 926</div>
        <div class="actions" style="display:flex;gap:8px">
          <button class="btn danger">数字不同</button>
          <button class="btn primary">数字一致，信任这台</button>
        </div>
      </div>
    </section>'''
    body = f'''<div class="os">
      <div style="position:absolute;inset:36px 40px 96px;display:flex;align-items:center;justify-content:space-between;gap:28px">
        {panel}
        {phone}
      </div>
      {dock()}
    </div>'''
    return page("配对", 1440, 900, body)


def handoff_desktop() -> str:
    banner = f'''
    <div class="banner">
      <div class="bubble" style="width:36px;height:36px;border-radius:12px;display:grid;place-items:center;background:rgba(20,150,106,.16);color:#d9ffe9">{ico("phone", 18)}</div>
      <div class="txt">
        <b>在手机上继续</b>
        <p>加密发送当前路径和未执行的草稿。确认不会跟着过去。像素不发送。</p>
      </div>
      <button class="btn ghost">不是现在</button>
      <button class="btn primary">继续</button>
    </div>'''
    win = f'''<div class="window" style="left:340px;top:72px;width:780px;height:560px">
      {titlebar("文件")}
      {banner}
      {files_body()}
    </div>'''
    body = f'''<div class="os">
      <div class="stage">{win}</div>
      {intent("把 README 里的安装步骤总结一下", "")}
      {dock("files")}
    </div>'''
    return page("在手机上继续", 1440, 900, body)


def handoff_phone() -> str:
    body = f'''<div class="phone">
      {phone_status()}
      <div style="padding:12px 16px 0">
        <p class="fine" style="margin:0">来自书桌 · 刚刚</p>
        <h1 style="font-size:28px;line-height:1.35;margin:8px 0">从书桌继续</h1>
        <div class="card" style="box-shadow:none">
          <b style="font-size:15px">文件 · /home/lin/w3cos</b>
          <p style="margin-top:6px">草稿还在：「把 README 里的安装步骤总结一下」</p>
          <p class="fine">L2 在这台手机上仍然关闭。要做的话在这里重新提议。书桌上的确认不会自动生效。</p>
        </div>
        <button class="btn primary block" style="margin-top:12px">打开文件</button>
      </div>
      <section class="sheet-m" role="dialog" aria-label="剪贴板冲突">
        <div class="grab"></div>
        <span class="tier l1">两份都还在</span>
        <h3 style="margin:8px 0 4px;font-size:18px">剪贴板有两份</h3>
        <div class="choice">
          <button>书桌 · 14:31<small>did:key:z6MkhaXg…ta2doK</small></button>
          <button>手机 · 14:32<small>https://w3cos.dev/docs</small></button>
        </div>
        <p class="fine">十分钟内可以改选。过期后留最后一次选用的那份。</p>
      </section>
    </div>'''
    return page("手机接续", 390, 844, body)


PAGES = {
    "desktop-home": desktop_home,
    "desktop-confirm": desktop_confirm,
    "desktop-permission": desktop_permission,
    "desktop-receipts": desktop_receipts,
    "desktop-lock": desktop_lock,
    "desktop-launcher": desktop_launcher,
    "desktop-notifications": desktop_notifications,
    "desktop-windows": desktop_windows,
    "state-empty": state_empty,
    "state-loading": state_loading,
    "state-error": state_error,
    "phone-home": phone_home,
    "phone-confirm": phone_confirm,
    "phone-switcher": phone_switcher,
    "phone-notifications": phone_notes,
    "tablet-home": tablet_home,
    "handoff-pair": handoff_pair,
    "handoff-desktop": handoff_desktop,
    "handoff-phone": handoff_phone,
}


def main() -> None:
    for name, fn in PAGES.items():
        path = OUT / f"{name}.html"
        path.write_text(fn(), encoding="utf-8")
        print(path.name)


if __name__ == "__main__":
    main()
