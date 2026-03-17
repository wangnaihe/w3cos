use crate::node::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    Click,
    MouseDown,
    MouseUp,
    MouseEnter,
    MouseLeave,
    KeyDown,
    KeyUp,
    Focus,
    Blur,
    Input,
    Change,
    Scroll,
    Resize,
}

impl EventType {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "click" => Some(Self::Click),
            "mousedown" => Some(Self::MouseDown),
            "mouseup" => Some(Self::MouseUp),
            "mouseenter" => Some(Self::MouseEnter),
            "mouseleave" => Some(Self::MouseLeave),
            "keydown" => Some(Self::KeyDown),
            "keyup" => Some(Self::KeyUp),
            "focus" => Some(Self::Focus),
            "blur" => Some(Self::Blur),
            "input" => Some(Self::Input),
            "change" => Some(Self::Change),
            "scroll" => Some(Self::Scroll),
            "resize" => Some(Self::Resize),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub event_type: EventType,
    pub target: NodeId,
    pub x: f32,
    pub y: f32,
    pub key: Option<String>,
    pub prevent_default: bool,
    pub stop_propagation: bool,
}

impl Event {
    pub fn click(target: NodeId, x: f32, y: f32) -> Self {
        Self {
            event_type: EventType::Click,
            target,
            x,
            y,
            key: None,
            prevent_default: false,
            stop_propagation: false,
        }
    }

    pub fn key(event_type: EventType, target: NodeId, key: impl Into<String>) -> Self {
        Self {
            event_type,
            target,
            x: 0.0,
            y: 0.0,
            key: Some(key.into()),
            prevent_default: false,
            stop_propagation: false,
        }
    }

    pub fn prevent_default(&mut self) {
        self.prevent_default = true;
    }

    pub fn stop_propagation(&mut self) {
        self.stop_propagation = true;
    }
}

pub type EventHandler = Box<dyn FnMut(&mut Event)>;

/// Per-node event listener storage.
pub struct EventListener {
    pub event_type: EventType,
    pub handler: EventHandler,
}

/// Registry of all event listeners in the document.
pub struct EventRegistry {
    listeners: Vec<(NodeId, EventListener)>,
}

impl EventRegistry {
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    pub fn add(&mut self, node: NodeId, event_type: EventType, handler: EventHandler) {
        self.listeners.push((
            node,
            EventListener {
                event_type,
                handler,
            },
        ));
    }

    pub fn remove_all(&mut self, node: NodeId) {
        self.listeners.retain(|(id, _)| *id != node);
    }

    pub fn dispatch(&mut self, event: &mut Event) {
        let target = event.target;
        for (node_id, listener) in self.listeners.iter_mut() {
            if *node_id == target && listener.event_type == event.event_type {
                (listener.handler)(event);
                if event.stop_propagation {
                    break;
                }
            }
        }
    }
}

impl Default for EventRegistry {
    fn default() -> Self {
        Self::new()
    }
}
