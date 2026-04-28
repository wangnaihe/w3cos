//! `w3cos.menu` — application menu bar + context menus.
//!
//! W3C OS draws the entire surface itself, so the menu module is a
//! cross-platform *data model* + *event delivery system* rather than an FFI
//! into native menu APIs. The shell or the application's renderer consumes
//! the [`MenuTree`] and surfaces it visually (menu bar, popup, tray, etc.);
//! when the user activates an item, the resulting [`MenuEvent`] is published
//! through a process-wide channel so signal-based code can react to it
//! without keeping closures alive across reactive renders.
//!
//! Mirrors Electron's `Menu` API:
//!
//! ```text
//! w3cos.menu.setApp([
//!   { label: "File", submenu: [
//!     { id: "open", label: "Open…", accelerator: "Cmd+O" },
//!     { type: "separator" },
//!     { id: "quit", label: "Quit", role: "quit" },
//!   ]}
//! ]);
//!
//! w3cos.menu.on("open", () => loadFile());
//! ```

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// A single entry in a menu tree.
#[derive(Debug, Clone, Default)]
pub struct MenuItem {
    /// Stable identifier emitted with [`MenuEvent`] when activated.
    pub id: Option<String>,
    /// Visible label. Empty for separators.
    pub label: String,
    /// Hint describing the item's semantic role: `"quit"`, `"about"`,
    /// `"copy"`, `"paste"`, etc. Renderers may map roles to platform
    /// conventions; the runtime treats them as opaque hints.
    pub role: Option<String>,
    /// Keyboard accelerator string, e.g. `"Cmd+O"` or `"Ctrl+Shift+P"`.
    pub accelerator: Option<String>,
    /// Tooltip / status text.
    pub tooltip: Option<String>,
    /// Submenu items. Empty when the item is a leaf.
    pub submenu: Vec<MenuItem>,
    /// Item kind (regular / separator / checkbox / radio).
    pub kind: MenuItemKind,
    /// Whether the item accepts input.
    pub enabled: bool,
    /// Whether the item is currently visible.
    pub visible: bool,
    /// Checked state for checkbox / radio items.
    pub checked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItemKind {
    Normal,
    Separator,
    Checkbox,
    Radio,
}

impl Default for MenuItemKind {
    fn default() -> Self {
        MenuItemKind::Normal
    }
}

impl MenuItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            label: label.into(),
            kind: MenuItemKind::Normal,
            enabled: true,
            visible: true,
            checked: false,
            ..Self::default()
        }
    }

    pub fn label_only(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: MenuItemKind::Normal,
            enabled: true,
            visible: true,
            ..Self::default()
        }
    }

    pub fn separator() -> Self {
        Self {
            kind: MenuItemKind::Separator,
            enabled: true,
            visible: true,
            ..Self::default()
        }
    }

    pub fn checkbox(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self {
            id: Some(id.into()),
            label: label.into(),
            kind: MenuItemKind::Checkbox,
            enabled: true,
            visible: true,
            checked,
            ..Self::default()
        }
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    pub fn with_accelerator(mut self, accelerator: impl Into<String>) -> Self {
        self.accelerator = Some(accelerator.into());
        self
    }

    pub fn with_submenu(mut self, items: Vec<MenuItem>) -> Self {
        self.submenu = items;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Find an item by id (depth-first, including submenus).
    pub fn find(&self, id: &str) -> Option<&MenuItem> {
        if self.id.as_deref() == Some(id) {
            return Some(self);
        }
        for child in &self.submenu {
            if let Some(found) = child.find(id) {
                return Some(found);
            }
        }
        None
    }
}

/// Top-level menu tree. The first level is the menu bar (or the root of a
/// context menu when invoked through [`pop_context`]).
#[derive(Debug, Clone, Default)]
pub struct MenuTree {
    pub items: Vec<MenuItem>,
}

impl MenuTree {
    pub fn new(items: Vec<MenuItem>) -> Self {
        Self { items }
    }

    pub fn find(&self, id: &str) -> Option<&MenuItem> {
        self.items.iter().find_map(|i| i.find(id))
    }

    pub fn ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        fn collect(item: &MenuItem, out: &mut Vec<String>) {
            if let Some(ref id) = item.id {
                out.push(id.clone());
            }
            for sub in &item.submenu {
                collect(sub, out);
            }
        }
        for item in &self.items {
            collect(item, &mut out);
        }
        out
    }
}

/// An activation emitted when the user picks an item from a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEvent {
    /// A normal / role-based menu item was clicked.
    Click { id: String },
    /// A checkbox / radio item changed state.
    Toggle { id: String, checked: bool },
    /// A context menu was dismissed without selection.
    Cancelled,
}

struct MenuRegistry {
    app_menu: Option<MenuTree>,
    context_menus: Vec<MenuTree>,
    events: VecDeque<MenuEvent>,
}

fn registry() -> &'static Mutex<MenuRegistry> {
    static REG: OnceLock<Mutex<MenuRegistry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(MenuRegistry {
            app_menu: None,
            context_menus: Vec::new(),
            events: VecDeque::new(),
        })
    })
}

/// `w3cos.menu.setApp(menu)` — install the application's menu bar.
pub fn set_app_menu(tree: MenuTree) {
    if let Ok(mut reg) = registry().lock() {
        reg.app_menu = Some(tree);
    }
}

/// Returns a clone of the current application menu (if any).
pub fn app_menu() -> Option<MenuTree> {
    registry().lock().ok().and_then(|r| r.app_menu.clone())
}

/// `w3cos.menu.popup(menu)` — push a context menu for the renderer to display.
/// The pop appears on the queue and is popped by the rendering layer when it
/// shows the menu UI.
pub fn pop_context(tree: MenuTree) -> usize {
    if let Ok(mut reg) = registry().lock() {
        reg.context_menus.push(tree);
        reg.context_menus.len() - 1
    } else {
        0
    }
}

/// Drain pending context menu requests. Renderers call this each frame.
pub fn take_pending_context_menus() -> Vec<MenuTree> {
    registry()
        .lock()
        .map(|mut r| std::mem::take(&mut r.context_menus))
        .unwrap_or_default()
}

/// Renderers / shells call this when the user activates a menu item.
pub fn dispatch_event(event: MenuEvent) {
    if let Ok(mut reg) = registry().lock() {
        reg.events.push_back(event);
    }
}

/// `w3cos.menu.on(id, handler)` — applications poll this from their reactive
/// loop instead of holding closures across renders.
pub fn poll_events() -> Vec<MenuEvent> {
    if let Ok(mut reg) = registry().lock() {
        reg.events.drain(..).collect()
    } else {
        Vec::new()
    }
}

/// Reset all menu state. Mainly used by tests.
pub fn reset() {
    if let Ok(mut reg) = registry().lock() {
        reg.app_menu = None;
        reg.context_menus.clear();
        reg.events.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn build_and_find() {
        let _g = TEST_GUARD.lock().unwrap();
        reset();

        let tree = MenuTree::new(vec![MenuItem::label_only("File").with_submenu(vec![
            MenuItem::new("open", "Open…").with_accelerator("Cmd+O"),
            MenuItem::separator(),
            MenuItem::new("quit", "Quit").with_role("quit"),
        ])]);
        set_app_menu(tree);

        let menu = app_menu().unwrap();
        assert_eq!(menu.items.len(), 1);
        assert!(menu.find("open").is_some());
        assert_eq!(menu.find("quit").unwrap().role.as_deref(), Some("quit"));
        let ids = menu.ids();
        assert!(ids.contains(&"open".to_string()));
        assert!(ids.contains(&"quit".to_string()));
    }

    #[test]
    fn dispatch_and_poll() {
        let _g = TEST_GUARD.lock().unwrap();
        reset();
        dispatch_event(MenuEvent::Click {
            id: "save".to_string(),
        });
        dispatch_event(MenuEvent::Toggle {
            id: "wrap".into(),
            checked: true,
        });
        let events = poll_events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], MenuEvent::Click { ref id } if id == "save"));
        assert!(matches!(
            events[1],
            MenuEvent::Toggle { ref id, checked } if id == "wrap" && checked
        ));
    }

    #[test]
    fn context_menu_queue() {
        let _g = TEST_GUARD.lock().unwrap();
        reset();
        let tree = MenuTree::new(vec![MenuItem::new("copy", "Copy")]);
        let _ = pop_context(tree);
        let menus = take_pending_context_menus();
        assert_eq!(menus.len(), 1);
        assert!(take_pending_context_menus().is_empty());
    }

    #[test]
    fn separator_and_checkbox() {
        let item = MenuItem::checkbox("wrap", "Word Wrap", true);
        assert_eq!(item.kind, MenuItemKind::Checkbox);
        assert!(item.checked);
        let sep = MenuItem::separator();
        assert_eq!(sep.kind, MenuItemKind::Separator);
    }
}
