//! System tray adapter for `navigator.w3cos.tray`.
//!
//! Desktop builds talk to the operating system through the `tray-icon`
//! crate:
//!
//! - Linux uses the StatusNotifierItem D-Bus interface (`ksni`). That is the
//!   protocol Ayatana AppIndicator implements. This runtime does not link
//!   GTK or `libappindicator`; a missing session bus is an error.
//! - macOS uses `NSStatusItem`. The icon has to be created on the main
//!   thread, and the shell event loop has to be running.
//! - Windows uses `Shell_NotifyIcon`. The creating thread has to pump Win32
//!   messages; the shell loop does that.
//!
//! A missing adapter, or an adapter that cannot reach the session, returns
//! an error. Callers reject that error. They do not report a successful
//! icon that was never installed. Tests install an in-memory recorder so
//! the JavaScript behavior can run without a display.

use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayMenuEntry {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
    pub separator: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayIconPixels {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraySpec {
    pub tooltip: String,
    pub title: String,
    pub menu: Vec<TrayMenuEntry>,
    pub icon: TrayIconPixels,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayEventKind {
    Click,
    Menu(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayEvent {
    pub id: u64,
    pub kind: TrayEventKind,
}

const ICON_SIDE: u32 = 16;

/// Built-in 16×16 status icon used when the page does not supply pixels.
pub fn default_icon() -> TrayIconPixels {
    let width = ICON_SIDE;
    let height = ICON_SIDE;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            let border = x == 0 || y == 0 || x + 1 == width || y + 1 == height;
            if border {
                rgba[index] = 29;
                rgba[index + 1] = 78;
                rgba[index + 2] = 137;
            } else {
                rgba[index] = 232;
                rgba[index + 1] = 240;
                rgba[index + 2] = 248;
            }
            rgba[index + 3] = 255;
        }
    }
    TrayIconPixels {
        rgba,
        width,
        height,
    }
}

/// Name of the adapter this build will call. `"none"` has no host backend.
pub fn backend_name() -> &'static str {
    #[cfg(all(feature = "host-tray", target_os = "linux", not(target_env = "ohos")))]
    {
        "StatusNotifierItem"
    }
    #[cfg(all(feature = "host-tray", target_os = "macos"))]
    {
        "NSStatusItem"
    }
    #[cfg(all(feature = "host-tray", target_os = "windows"))]
    {
        "Shell_NotifyIcon"
    }
    #[cfg(not(all(
        feature = "host-tray",
        any(target_os = "macos", target_os = "linux", target_os = "windows"),
        not(target_env = "ohos")
    )))]
    {
        "none"
    }
}

pub fn create(spec: &TraySpec) -> Result<u64, String> {
    check_spec(spec)?;
    #[cfg(test)]
    if let Some(id) = recorder_create(spec) {
        return id;
    }
    create_native(spec)
}

pub fn update(id: u64, spec: &TraySpec) -> Result<(), String> {
    check_spec(spec)?;
    #[cfg(test)]
    if let Some(result) = recorder_update(id, spec) {
        return result;
    }
    update_native(id, spec)
}

pub fn remove(id: u64) -> Result<(), String> {
    #[cfg(test)]
    if let Some(result) = recorder_remove(id) {
        return result;
    }
    remove_native(id)
}

pub fn poll_events() -> Vec<TrayEvent> {
    #[cfg(test)]
    if let Some(events) = recorder_poll() {
        return events;
    }
    poll_native()
}

pub fn reset() {
    #[cfg(test)]
    recorder_reset();
    clear_native();
    // Drop events that belonged to icons this reset just removed.
    let _ = poll_events();
}

fn check_spec(spec: &TraySpec) -> Result<(), String> {
    let pixels = (spec.icon.width as usize)
        .checked_mul(spec.icon.height as usize)
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| "tray icon is too large".to_string())?;
    if spec.icon.width == 0
        || spec.icon.height == 0
        || spec.icon.width > 128
        || spec.icon.height > 128
        || spec.icon.rgba.len() != pixels
    {
        return Err("tray icon rgba does not match its dimensions".into());
    }
    Ok(())
}

#[cfg(not(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn no_adapter() -> String {
    "navigator.w3cos.tray has no W3C draft and no platform adapter; the call was refused".into()
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn adapter_error(error: impl std::fmt::Display) -> String {
    format!("{} adapter failed: {error}", backend_name())
}

#[cfg(test)]
struct Recorder {
    icons: HashMap<u64, TraySpec>,
    next_id: u64,
    events: Vec<TrayEvent>,
}

#[cfg(test)]
thread_local! {
    static RECORDER: RefCell<Option<Recorder>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub fn install_recorder() {
    RECORDER.with(|slot| {
        *slot.borrow_mut() = Some(Recorder {
            icons: HashMap::new(),
            next_id: 1,
            events: Vec::new(),
        });
    });
}

#[cfg(test)]
pub fn recorder_spec(id: u64) -> Option<TraySpec> {
    RECORDER.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|recorder| recorder.icons.get(&id).cloned())
    })
}

#[cfg(test)]
pub fn push_recorder_event(id: u64, kind: TrayEventKind) {
    RECORDER.with(|slot| {
        if let Some(recorder) = slot.borrow_mut().as_mut() {
            recorder.events.push(TrayEvent { id, kind });
        }
    });
}

#[cfg(test)]
fn recorder_create(spec: &TraySpec) -> Option<Result<u64, String>> {
    RECORDER.with(|slot| {
        let mut recorder_slot = slot.borrow_mut();
        let recorder = recorder_slot.as_mut()?;
        let id = recorder.next_id;
        recorder.next_id = recorder.next_id.saturating_add(1);
        recorder.icons.insert(id, spec.clone());
        Some(Ok(id))
    })
}

#[cfg(test)]
fn recorder_update(id: u64, spec: &TraySpec) -> Option<Result<(), String>> {
    RECORDER.with(|slot| {
        let mut recorder_slot = slot.borrow_mut();
        let recorder = recorder_slot.as_mut()?;
        if !recorder.icons.contains_key(&id) {
            return Some(Err("tray icon was removed".into()));
        }
        recorder.icons.insert(id, spec.clone());
        Some(Ok(()))
    })
}

#[cfg(test)]
fn recorder_remove(id: u64) -> Option<Result<(), String>> {
    RECORDER.with(|slot| {
        let mut recorder_slot = slot.borrow_mut();
        let recorder = recorder_slot.as_mut()?;
        if recorder.icons.remove(&id).is_none() {
            return Some(Err("tray icon was removed".into()));
        }
        Some(Ok(()))
    })
}

#[cfg(test)]
fn recorder_poll() -> Option<Vec<TrayEvent>> {
    RECORDER.with(|slot| {
        let mut recorder_slot = slot.borrow_mut();
        let recorder = recorder_slot.as_mut()?;
        Some(std::mem::take(&mut recorder.events))
    })
}

#[cfg(test)]
fn recorder_reset() {
    RECORDER.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
mod native {
    use super::{TrayEvent, TrayEventKind, TrayMenuEntry, TraySpec, adapter_error};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use tray_icon::menu::{
        CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
    };
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    struct NativeIcon {
        icon: TrayIcon,
    }

    thread_local! {
        static ICONS: RefCell<HashMap<u64, NativeIcon>> = RefCell::new(HashMap::new());
        static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    }

    const MENU_SPLIT: char = '\u{1f}';

    pub fn create(spec: &TraySpec) -> Result<u64, String> {
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id.saturating_add(1));
            id
        });
        let icon = build(id, spec)?;
        ICONS.with(|icons| {
            icons.borrow_mut().insert(id, NativeIcon { icon });
        });
        Ok(id)
    }

    pub fn update(id: u64, spec: &TraySpec) -> Result<(), String> {
        let menu = build_menu(id, &spec.menu)?;
        let pixels = icon_from(spec)?;
        ICONS.with(|icons| {
            let icons = icons.borrow();
            let Some(native) = icons.get(&id) else {
                return Err("tray icon was removed".into());
            };
            native
                .icon
                .set_tooltip(Some(spec.tooltip.as_str()))
                .map_err(adapter_error)?;
            native.icon.set_title(Some(spec.title.as_str()));
            native.icon.set_icon(Some(pixels)).map_err(adapter_error)?;
            native.icon.set_menu(menu);
            Ok(())
        })
    }

    pub fn remove(id: u64) -> Result<(), String> {
        let removed = ICONS.with(|icons| icons.borrow_mut().remove(&id));
        if removed.is_none() {
            return Err("tray icon was removed".into());
        }
        Ok(())
    }

    pub fn clear() {
        ICONS.with(|icons| icons.borrow_mut().clear());
        NEXT_ID.with(|next| next.set(1));
    }

    pub fn poll() -> Vec<TrayEvent> {
        let mut events = Vec::new();
        let tray_receiver = TrayIconEvent::receiver();
        while let Ok(event) = tray_receiver.try_recv() {
            if let Some(event) = map_tray_event(event) {
                events.push(event);
            }
        }
        let menu_receiver = MenuEvent::receiver();
        while let Ok(event) = menu_receiver.try_recv() {
            if let Some(event) = map_menu_event(&event.id.0) {
                events.push(event);
            }
        }
        events
    }

    fn build(id: u64, spec: &TraySpec) -> Result<TrayIcon, String> {
        let menu = build_menu(id, &spec.menu)?;
        let pixels = icon_from(spec)?;
        let mut builder = TrayIconBuilder::new()
            .with_id(id.to_string())
            .with_icon(pixels)
            .with_menu_on_left_click(false);
        if !spec.tooltip.is_empty() {
            builder = builder.with_tooltip(spec.tooltip.as_str());
        }
        if !spec.title.is_empty() {
            builder = builder.with_title(spec.title.as_str());
        }
        if let Some(menu) = menu {
            builder = builder.with_menu(menu);
        }
        builder.build().map_err(adapter_error)
    }

    fn icon_from(spec: &TraySpec) -> Result<Icon, String> {
        Icon::from_rgba(spec.icon.rgba.clone(), spec.icon.width, spec.icon.height)
            .map_err(adapter_error)
    }

    fn build_menu(
        tray_id: u64,
        entries: &[TrayMenuEntry],
    ) -> Result<Option<Box<dyn ContextMenu>>, String> {
        if entries.is_empty() {
            return Ok(None);
        }
        let menu = Menu::new();
        for entry in entries {
            if entry.separator {
                menu.append(&PredefinedMenuItem::separator())
                    .map_err(adapter_error)?;
                continue;
            }
            let token = format!("{tray_id}{MENU_SPLIT}{}", entry.id);
            let label = entry.label.replace('&', "&&");
            if entry.checked {
                let item = CheckMenuItem::with_id(token, label, entry.enabled, true, None);
                menu.append(&item).map_err(adapter_error)?;
            } else {
                let item = MenuItem::with_id(token, label, entry.enabled, None);
                menu.append(&item).map_err(adapter_error)?;
            }
        }
        Ok(Some(Box::new(menu)))
    }

    fn map_tray_event(event: TrayIconEvent) -> Option<TrayEvent> {
        match event {
            TrayIconEvent::Click {
                id,
                button,
                button_state,
                ..
            } if button == MouseButton::Left && button_state == MouseButtonState::Up => {
                parse_id(&id).map(|id| TrayEvent {
                    id,
                    kind: TrayEventKind::Click,
                })
            }
            TrayIconEvent::DoubleClick { id, button, .. } if button == MouseButton::Left => {
                parse_id(&id).map(|id| TrayEvent {
                    id,
                    kind: TrayEventKind::Click,
                })
            }
            _ => None,
        }
    }

    fn map_menu_event(token: &str) -> Option<TrayEvent> {
        let (tray_id, item_id) = token.split_once(MENU_SPLIT)?;
        let id = tray_id.parse().ok()?;
        if item_id.is_empty() {
            return None;
        }
        Some(TrayEvent {
            id,
            kind: TrayEventKind::Menu(item_id.to_string()),
        })
    }

    fn parse_id(id: &tray_icon::TrayIconId) -> Option<u64> {
        id.as_ref().parse().ok()
    }
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn create_native(spec: &TraySpec) -> Result<u64, String> {
    native::create(spec)
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn update_native(id: u64, spec: &TraySpec) -> Result<(), String> {
    native::update(id, spec)
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn remove_native(id: u64) -> Result<(), String> {
    native::remove(id)
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn clear_native() {
    native::clear();
}

#[cfg(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
fn poll_native() -> Vec<TrayEvent> {
    native::poll()
}

#[cfg(not(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn create_native(_spec: &TraySpec) -> Result<u64, String> {
    Err(no_adapter())
}

#[cfg(not(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn update_native(_id: u64, _spec: &TraySpec) -> Result<(), String> {
    Err(no_adapter())
}

#[cfg(not(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn remove_native(_id: u64) -> Result<(), String> {
    Err(no_adapter())
}

#[cfg(not(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn clear_native() {}

#[cfg(not(all(
    feature = "host-tray",
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
fn poll_native() -> Vec<TrayEvent> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_icon_is_opaque_square_rgba() {
        let icon = default_icon();
        assert_eq!(icon.width, 16);
        assert_eq!(icon.height, 16);
        assert_eq!(icon.rgba.len(), 16 * 16 * 4);
        assert!(icon.rgba.chunks(4).all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn backend_name_matches_the_compiled_adapter() {
        #[cfg(all(feature = "host-tray", target_os = "linux", not(target_env = "ohos")))]
        assert_eq!(backend_name(), "StatusNotifierItem");
        #[cfg(all(feature = "host-tray", target_os = "macos"))]
        assert_eq!(backend_name(), "NSStatusItem");
        #[cfg(all(feature = "host-tray", target_os = "windows"))]
        assert_eq!(backend_name(), "Shell_NotifyIcon");
        #[cfg(not(all(
            feature = "host-tray",
            any(target_os = "macos", target_os = "linux", target_os = "windows"),
            not(target_env = "ohos")
        )))]
        assert_eq!(backend_name(), "none");
    }

    #[test]
    fn recorder_create_update_and_remove_do_not_need_a_display() {
        reset();
        install_recorder();
        let mut spec = TraySpec {
            tooltip: "Build".into(),
            title: "W3C OS".into(),
            menu: vec![TrayMenuEntry {
                id: "quit".into(),
                label: "Quit".into(),
                enabled: true,
                checked: false,
                separator: false,
            }],
            icon: default_icon(),
        };
        let id = create(&spec).expect("recorder create");
        assert_eq!(recorder_spec(id).unwrap().tooltip, "Build");
        spec.tooltip = "Updated".into();
        update(id, &spec).expect("recorder update");
        assert_eq!(recorder_spec(id).unwrap().tooltip, "Updated");
        push_recorder_event(id, TrayEventKind::Click);
        push_recorder_event(id, TrayEventKind::Menu("quit".into()));
        let events = poll_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, TrayEventKind::Click);
        assert_eq!(events[1].kind, TrayEventKind::Menu("quit".into()));
        remove(id).expect("recorder remove");
        assert!(recorder_spec(id).is_none());
        assert!(update(id, &spec).is_err());
        reset();
    }
}
