use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_dom::Document;
use w3cos_std::Component;

use crate::manifest::{AppManifest, AppRegistry, W3cosUrl, WindowConfig};

/// Unique window identifier.
pub type WinId = u32;

/// A single application window — each has its own Document, Component tree, and layout state.
pub struct AppWindow {
    pub id: WinId,
    pub app_id: String,
    pub title: String,
    pub document: Document,
    pub component_root: Component,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub visible: bool,
    pub focused: bool,
    pub closed: bool,
    pub opener: Option<WinId>,
    pub url: String,
    pub message_queue: Vec<WindowMessage>,
}

/// A postMessage payload waiting to be delivered.
#[derive(Debug, Clone)]
pub struct WindowMessage {
    pub data: String,
    pub source_window: WinId,
    pub origin: String,
}

impl AppWindow {
    pub fn new(id: WinId, app_id: &str, config: &WindowConfig) -> Self {
        let doc = Document::new();
        let root = doc.to_component_tree();
        Self {
            id,
            app_id: app_id.to_string(),
            title: config.title.clone().unwrap_or_else(|| app_id.to_string()),
            document: doc,
            component_root: root,
            x: 100 + (id as i32 * 30),
            y: 100 + (id as i32 * 30),
            width: config.default_width,
            height: config.default_height,
            resizable: config.resizable,
            visible: true,
            focused: false,
            closed: false,
            opener: None,
            url: format!("w3cos://{}", app_id),
            message_queue: Vec::new(),
        }
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.visible = false;
    }

    pub fn move_to(&mut self, x: i32, y: i32) {
        self.x = x;
        self.y = y;
    }

    pub fn resize_to(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }
}

/// Manages all open windows — the equivalent of a window manager + compositor.
pub struct WindowManager {
    windows: HashMap<WinId, AppWindow>,
    focus_stack: Vec<WinId>,
    next_id: WinId,
    pub registry: AppRegistry,
}

impl WindowManager {
    pub fn new() -> Self {
        let mut registry = AppRegistry::new();
        registry.register_builtins();
        Self {
            windows: HashMap::new(),
            focus_stack: Vec::new(),
            next_id: 1,
            registry,
        }
    }

    /// Open a new window via standard window.open(url) semantics.
    /// Parses the w3cos:// URL to determine which app to launch.
    pub fn open(&mut self, url: &str, opener: Option<WinId>) -> Option<WinId> {
        let parsed = W3cosUrl::parse(url)?;
        let config = self
            .registry
            .get(&parsed.app_id)
            .map(|m| m.window.clone())
            .unwrap_or_default();

        let id = self.next_id;
        self.next_id += 1;

        let mut win = AppWindow::new(id, &parsed.app_id, &config);
        win.opener = opener;
        win.url = url.to_string();
        win.focused = true;

        // Unfocus previously focused window
        if let Some(&top) = self.focus_stack.last() {
            if let Some(prev) = self.windows.get_mut(&top) {
                prev.focused = false;
            }
        }

        self.focus_stack.push(id);
        self.windows.insert(id, win);
        Some(id)
    }

    /// Close a window. Standard window.close() semantics.
    pub fn close(&mut self, id: WinId) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.close();
        }
        self.focus_stack.retain(|&wid| wid != id);
        // Focus next window
        if let Some(&top) = self.focus_stack.last() {
            if let Some(win) = self.windows.get_mut(&top) {
                win.focused = true;
            }
        }
    }

    /// Focus a window. Standard window.focus() semantics.
    pub fn focus(&mut self, id: WinId) {
        // Unfocus current top
        if let Some(&top) = self.focus_stack.last() {
            if let Some(win) = self.windows.get_mut(&top) {
                win.focused = false;
            }
        }
        // Move to top of stack
        self.focus_stack.retain(|&wid| wid != id);
        self.focus_stack.push(id);
        if let Some(win) = self.windows.get_mut(&id) {
            win.focused = true;
        }
    }

    /// Standard postMessage: send data from one window to another.
    pub fn post_message(&mut self, target: WinId, data: &str, source: WinId) {
        let origin = self
            .windows
            .get(&source)
            .map(|w| format!("w3cos://{}", w.app_id))
            .unwrap_or_default();

        if let Some(win) = self.windows.get_mut(&target) {
            win.message_queue.push(WindowMessage {
                data: data.to_string(),
                source_window: source,
                origin,
            });
        }
    }

    /// Drain pending messages for a window (called by event loop).
    pub fn take_messages(&mut self, id: WinId) -> Vec<WindowMessage> {
        self.windows
            .get_mut(&id)
            .map(|w| std::mem::take(&mut w.message_queue))
            .unwrap_or_default()
    }

    pub fn get(&self, id: WinId) -> Option<&AppWindow> {
        self.windows.get(&id)
    }

    pub fn get_mut(&mut self, id: WinId) -> Option<&mut AppWindow> {
        self.windows.get_mut(&id)
    }

    pub fn focused_window(&self) -> Option<WinId> {
        self.focus_stack.last().copied()
    }

    /// All visible windows in z-order (bottom to top).
    pub fn visible_windows(&self) -> Vec<WinId> {
        self.focus_stack
            .iter()
            .filter(|&&id| {
                self.windows
                    .get(&id)
                    .map(|w| w.visible && !w.closed)
                    .unwrap_or(false)
            })
            .copied()
            .collect()
    }

    /// All open (not closed) windows.
    pub fn all_windows(&self) -> Vec<WinId> {
        self.windows
            .keys()
            .filter(|&&id| {
                self.windows
                    .get(&id)
                    .map(|w| !w.closed)
                    .unwrap_or(false)
            })
            .copied()
            .collect()
    }

    /// Clean up closed windows.
    pub fn gc_closed(&mut self) {
        let closed: Vec<WinId> = self
            .windows
            .iter()
            .filter(|(_, w)| w.closed)
            .map(|(&id, _)| id)
            .collect();
        for id in closed {
            self.windows.remove(&id);
        }
        self.focus_stack
            .retain(|id| self.windows.contains_key(id));
    }
}

impl Default for WindowManager {
    fn default() -> Self {
        Self::new()
    }
}

// Thread-local WindowManager for runtime access
thread_local! {
    static WM: RefCell<WindowManager> = RefCell::new(WindowManager::new());
}

/// Open a new window. Returns the window ID.
/// Standard: `window.open("w3cos://files/home/user")`
pub fn window_open(url: &str, opener: Option<WinId>) -> Option<WinId> {
    WM.with(|wm| wm.borrow_mut().open(url, opener))
}

/// Close a window. Standard: `window.close()`
pub fn window_close(id: WinId) {
    WM.with(|wm| wm.borrow_mut().close(id))
}

/// Focus a window. Standard: `window.focus()`
pub fn window_focus(id: WinId) {
    WM.with(|wm| wm.borrow_mut().focus(id))
}

/// Move a window. Standard: `window.moveTo(x, y)`
pub fn window_move_to(id: WinId, x: i32, y: i32) {
    WM.with(|wm| {
        if let Some(win) = wm.borrow_mut().get_mut(id) {
            win.move_to(x, y);
        }
    })
}

/// Resize a window. Standard: `window.resizeTo(w, h)`
pub fn window_resize_to(id: WinId, width: u32, height: u32) {
    WM.with(|wm| {
        if let Some(win) = wm.borrow_mut().get_mut(id) {
            win.resize_to(width, height);
        }
    })
}

/// Standard: `otherWindow.postMessage(data, origin)`
pub fn window_post_message(target: WinId, data: &str, source: WinId) {
    WM.with(|wm| wm.borrow_mut().post_message(target, data, source))
}

/// Check if a window is closed. Standard: `window.closed`
pub fn window_closed(id: WinId) -> bool {
    WM.with(|wm| {
        wm.borrow()
            .get(id)
            .map(|w| w.closed)
            .unwrap_or(true)
    })
}

/// Get the opener window ID. Standard: `window.opener`
pub fn window_opener(id: WinId) -> Option<WinId> {
    WM.with(|wm| wm.borrow().get(id).and_then(|w| w.opener))
}

/// Get window title.
pub fn window_title(id: WinId) -> String {
    WM.with(|wm| {
        wm.borrow()
            .get(id)
            .map(|w| w.title.clone())
            .unwrap_or_default()
    })
}

/// Get all visible window IDs in z-order.
pub fn visible_windows() -> Vec<WinId> {
    WM.with(|wm| wm.borrow().visible_windows())
}

/// Get currently focused window.
pub fn focused_window() -> Option<WinId> {
    WM.with(|wm| wm.borrow().focused_window())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_close_window() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let id = window_open("w3cos://files", None).unwrap();
        assert!(!window_closed(id));
        assert_eq!(focused_window(), Some(id));

        window_close(id);
        assert!(window_closed(id));
    }

    #[test]
    fn multiple_windows_focus() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let w1 = window_open("w3cos://files", None).unwrap();
        let w2 = window_open("w3cos://terminal", None).unwrap();

        assert_eq!(focused_window(), Some(w2));

        window_focus(w1);
        assert_eq!(focused_window(), Some(w1));
    }

    #[test]
    fn window_opener_chain() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let parent = window_open("w3cos://shell", None).unwrap();
        let child = window_open("w3cos://settings", Some(parent)).unwrap();

        assert_eq!(window_opener(child), Some(parent));
        assert_eq!(window_opener(parent), None);
    }

    #[test]
    fn post_message_delivery() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let w1 = window_open("w3cos://files", None).unwrap();
        let w2 = window_open("w3cos://editor", None).unwrap();

        window_post_message(w2, r#"{"type":"open","file":"app.tsx"}"#, w1);

        let msgs = WM.with(|wm| wm.borrow_mut().take_messages(w2));
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].data.contains("app.tsx"));
        assert_eq!(msgs[0].source_window, w1);
        assert_eq!(msgs[0].origin, "w3cos://files");
    }

    #[test]
    fn move_and_resize() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let id = window_open("w3cos://terminal", None).unwrap();
        window_move_to(id, 200, 150);
        window_resize_to(id, 800, 600);

        WM.with(|wm| {
            let binding = wm.borrow();
            let win = binding.get(id).unwrap();
            assert_eq!(win.x, 200);
            assert_eq!(win.y, 150);
            assert_eq!(win.width, 800);
            assert_eq!(win.height, 600);
        });
    }

    #[test]
    fn visible_windows_order() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let w1 = window_open("w3cos://files", None).unwrap();
        let w2 = window_open("w3cos://terminal", None).unwrap();
        let w3 = window_open("w3cos://settings", None).unwrap();

        let visible = visible_windows();
        assert_eq!(visible, vec![w1, w2, w3]);

        window_close(w2);
        let visible = visible_windows();
        assert_eq!(visible, vec![w1, w3]);
    }

    #[test]
    fn window_title() {
        WM.with(|wm| *wm.borrow_mut() = WindowManager::new());

        let id = window_open("w3cos://files", None).unwrap();
        assert_eq!(super::window_title(id), "files");
    }
}
