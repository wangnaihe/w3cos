use crate::node::NodeId;

// ── Event Types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    Click,
    DblClick,
    ContextMenu,
    MouseDown,
    MouseUp,
    MouseMove,
    MouseEnter,
    MouseLeave,
    MouseOver,
    MouseOut,
    PointerDown,
    PointerUp,
    PointerMove,
    PointerEnter,
    PointerLeave,
    PointerOver,
    PointerOut,
    PointerCancel,
    KeyDown,
    KeyUp,
    KeyPress,
    Focus,
    Blur,
    FocusIn,
    FocusOut,
    Input,
    Change,
    Scroll,
    Wheel,
    Resize,
    TouchStart,
    TouchEnd,
    TouchMove,
    TouchCancel,
    CompositionStart,
    CompositionUpdate,
    CompositionEnd,
    PopState,
    HashChange,
    Custom(u32),
}

static CUSTOM_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl EventType {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "click" => Some(Self::Click),
            "dblclick" => Some(Self::DblClick),
            "contextmenu" => Some(Self::ContextMenu),
            "mousedown" => Some(Self::MouseDown),
            "mouseup" => Some(Self::MouseUp),
            "mousemove" => Some(Self::MouseMove),
            "mouseenter" => Some(Self::MouseEnter),
            "mouseleave" => Some(Self::MouseLeave),
            "mouseover" => Some(Self::MouseOver),
            "mouseout" => Some(Self::MouseOut),
            "pointerdown" => Some(Self::PointerDown),
            "pointerup" => Some(Self::PointerUp),
            "pointermove" => Some(Self::PointerMove),
            "pointerenter" => Some(Self::PointerEnter),
            "pointerleave" => Some(Self::PointerLeave),
            "pointerover" => Some(Self::PointerOver),
            "pointerout" => Some(Self::PointerOut),
            "pointercancel" => Some(Self::PointerCancel),
            "keydown" => Some(Self::KeyDown),
            "keyup" => Some(Self::KeyUp),
            "keypress" => Some(Self::KeyPress),
            "focus" => Some(Self::Focus),
            "blur" => Some(Self::Blur),
            "focusin" => Some(Self::FocusIn),
            "focusout" => Some(Self::FocusOut),
            "input" => Some(Self::Input),
            "change" => Some(Self::Change),
            "scroll" => Some(Self::Scroll),
            "wheel" => Some(Self::Wheel),
            "resize" => Some(Self::Resize),
            "touchstart" => Some(Self::TouchStart),
            "touchend" => Some(Self::TouchEnd),
            "touchmove" => Some(Self::TouchMove),
            "touchcancel" => Some(Self::TouchCancel),
            "compositionstart" => Some(Self::CompositionStart),
            "compositionupdate" => Some(Self::CompositionUpdate),
            "compositionend" => Some(Self::CompositionEnd),
            "popstate" => Some(Self::PopState),
            "hashchange" => Some(Self::HashChange),
            _ => {
                let id = CUSTOM_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(Self::Custom(id))
            }
        }
    }

    pub fn bubbles(&self) -> bool {
        !matches!(
            self,
            EventType::Focus
                | EventType::Blur
                | EventType::MouseEnter
                | EventType::MouseLeave
                | EventType::PointerEnter
                | EventType::PointerLeave
                | EventType::Resize
        )
    }
}

// ── Event Data Sub-types ───────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MouseEventData {
    pub client_x: f64,
    pub client_y: f64,
    pub page_x: f64,
    pub page_y: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    pub button: u16,
    pub buttons: u16,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub meta_key: bool,
}

#[derive(Debug, Clone, Default)]
pub struct KeyboardEventData {
    pub key: String,
    pub code: String,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub meta_key: bool,
    pub repeat: bool,
    pub location: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PointerEventData {
    pub mouse: MouseEventData,
    pub pointer_id: i32,
    pub pointer_type: String,
    pub pressure: f32,
    pub width: f32,
    pub height: f32,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WheelEventData {
    pub mouse: MouseEventData,
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_z: f64,
    pub delta_mode: u32,
}

#[derive(Debug, Clone)]
pub enum EventData {
    Mouse(MouseEventData),
    Keyboard(KeyboardEventData),
    Pointer(PointerEventData),
    Wheel(WheelEventData),
    Focus,
    Input { data: Option<String> },
    Composition { data: String },
    Custom { detail: Option<String> },
    None,
}

impl Default for EventData {
    fn default() -> Self {
        EventData::None
    }
}

// ── Event Phase ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventPhase {
    None = 0,
    Capturing = 1,
    AtTarget = 2,
    Bubbling = 3,
}

// ── Event ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Event {
    pub event_type: EventType,
    pub target: NodeId,
    pub current_target: NodeId,
    pub event_phase: EventPhase,
    pub bubbles: bool,
    pub cancelable: bool,
    pub composed: bool,
    pub timestamp: f64,
    pub data: EventData,
    pub prevent_default: bool,
    pub stop_propagation: bool,
    pub stop_immediate_propagation: bool,
    // Legacy compat fields
    pub x: f32,
    pub y: f32,
    pub key: Option<String>,
}

impl Event {
    pub fn new(event_type: EventType, target: NodeId) -> Self {
        Self {
            bubbles: event_type.bubbles(),
            cancelable: true,
            event_type,
            target,
            current_target: target,
            event_phase: EventPhase::None,
            composed: false,
            timestamp: 0.0,
            data: EventData::None,
            prevent_default: false,
            stop_propagation: false,
            stop_immediate_propagation: false,
            x: 0.0,
            y: 0.0,
            key: None,
        }
    }

    pub fn click(target: NodeId, x: f32, y: f32) -> Self {
        let mut ev = Self::new(EventType::Click, target);
        ev.x = x;
        ev.y = y;
        ev.data = EventData::Mouse(MouseEventData {
            client_x: x as f64,
            client_y: y as f64,
            page_x: x as f64,
            page_y: y as f64,
            ..Default::default()
        });
        ev
    }

    pub fn mouse(event_type: EventType, target: NodeId, data: MouseEventData) -> Self {
        let mut ev = Self::new(event_type, target);
        ev.x = data.client_x as f32;
        ev.y = data.client_y as f32;
        ev.data = EventData::Mouse(data);
        ev
    }

    pub fn keyboard(event_type: EventType, target: NodeId, data: KeyboardEventData) -> Self {
        let mut ev = Self::new(event_type, target);
        ev.key = Some(data.key.clone());
        ev.data = EventData::Keyboard(data);
        ev
    }

    pub fn pointer(event_type: EventType, target: NodeId, data: PointerEventData) -> Self {
        let mut ev = Self::new(event_type, target);
        ev.x = data.mouse.client_x as f32;
        ev.y = data.mouse.client_y as f32;
        ev.data = EventData::Pointer(data);
        ev
    }

    pub fn wheel(target: NodeId, data: WheelEventData) -> Self {
        let mut ev = Self::new(EventType::Wheel, target);
        ev.data = EventData::Wheel(data);
        ev
    }

    pub fn key(event_type: EventType, target: NodeId, key: impl Into<String>) -> Self {
        let key_str = key.into();
        let mut ev = Self::new(event_type, target);
        ev.key = Some(key_str.clone());
        ev.data = EventData::Keyboard(KeyboardEventData {
            key: key_str,
            ..Default::default()
        });
        ev
    }

    pub fn custom(type_name: &str, target: NodeId, detail: Option<String>) -> Self {
        let event_type = EventType::from_str(type_name).unwrap_or(EventType::Custom(0));
        let mut ev = Self::new(event_type, target);
        ev.data = EventData::Custom { detail };
        ev
    }

    pub fn prevent_default(&mut self) {
        self.prevent_default = true;
    }

    pub fn stop_propagation(&mut self) {
        self.stop_propagation = true;
    }

    pub fn stop_immediate_propagation(&mut self) {
        self.stop_immediate_propagation = true;
        self.stop_propagation = true;
    }

    // ── Accessor helpers for event data ──

    pub fn mouse_data(&self) -> Option<&MouseEventData> {
        match &self.data {
            EventData::Mouse(d) => Some(d),
            EventData::Pointer(d) => Some(&d.mouse),
            EventData::Wheel(d) => Some(&d.mouse),
            _ => None,
        }
    }

    pub fn keyboard_data(&self) -> Option<&KeyboardEventData> {
        match &self.data {
            EventData::Keyboard(d) => Some(d),
            _ => None,
        }
    }

    pub fn pointer_data(&self) -> Option<&PointerEventData> {
        match &self.data {
            EventData::Pointer(d) => Some(d),
            _ => None,
        }
    }
}

// ── Listener Options ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct ListenerOptions {
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
}

pub type EventHandler = Box<dyn FnMut(&mut Event)>;

pub struct EventListener {
    pub id: u32,
    pub event_type: EventType,
    pub handler: EventHandler,
    pub options: ListenerOptions,
}

// ── Event Registry ─────────────────────────────────────────────────────

pub struct EventRegistry {
    listeners: Vec<(NodeId, EventListener)>,
    next_id: u32,
}

impl EventRegistry {
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
            next_id: 1,
        }
    }

    pub fn add(&mut self, node: NodeId, event_type: EventType, handler: EventHandler) -> u32 {
        self.add_with_options(node, event_type, handler, ListenerOptions::default())
    }

    pub fn add_with_options(
        &mut self,
        node: NodeId,
        event_type: EventType,
        handler: EventHandler,
        options: ListenerOptions,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.listeners.push((
            node,
            EventListener {
                id,
                event_type,
                handler,
                options,
            },
        ));
        id
    }

    pub fn remove(&mut self, node: NodeId, listener_id: u32) {
        self.listeners
            .retain(|(id, l)| !(*id == node && l.id == listener_id));
    }

    pub fn remove_by_type(&mut self, node: NodeId, event_type: EventType) {
        self.listeners
            .retain(|(id, l)| !(*id == node && l.event_type == event_type));
    }

    pub fn remove_all(&mut self, node: NodeId) {
        self.listeners.retain(|(id, _)| *id != node);
    }

    /// Dispatch event to listeners on the target node only (no bubbling).
    pub fn dispatch(&mut self, event: &mut Event) {
        self.dispatch_at_node(event.target, event);
    }

    /// Dispatch event to listeners on a specific node.
    pub fn dispatch_at_node(&mut self, node_id: NodeId, event: &mut Event) {
        let old_current = event.current_target;
        event.current_target = node_id;

        let mut once_ids = Vec::new();

        for (listener_node, listener) in self.listeners.iter_mut() {
            if *listener_node != node_id || listener.event_type != event.event_type {
                continue;
            }
            let phase_match = match event.event_phase {
                EventPhase::Capturing => listener.options.capture,
                EventPhase::Bubbling => !listener.options.capture,
                EventPhase::AtTarget | EventPhase::None => true,
            };
            if !phase_match {
                continue;
            }
            (listener.handler)(event);
            if listener.options.once {
                once_ids.push(listener.id);
            }
            if event.stop_immediate_propagation {
                break;
            }
        }

        for id in once_ids {
            self.listeners.retain(|(_, l)| l.id != id);
        }

        event.current_target = old_current;
    }
}

impl Default for EventRegistry {
    fn default() -> Self {
        Self::new()
    }
}
