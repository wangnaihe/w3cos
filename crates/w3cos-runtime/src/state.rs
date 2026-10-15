use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use w3cos_dom::host_runtime;
use w3cos_std::{Component, EventAction};

/// Thread-local reactive signal store.
/// Signals hold i64 values that can be read and modified by event handlers.
/// When any signal changes, the `dirty` flag is set, prompting a UI rebuild.
pub(crate) struct SignalStore {
    values: Vec<i64>,
    text_values: HashMap<usize, String>,
    pub(crate) dirty: bool,
}

impl SignalStore {
    fn new() -> Self {
        Self {
            values: Vec::new(),
            text_values: HashMap::new(),
            dirty: false,
        }
    }
}

thread_local! {
    pub(crate) static STORE: RefCell<SignalStore> = RefCell::new(SignalStore::new());
    static SIGNALS_REGISTERED: RefCell<bool> = RefCell::new(false);
    static SIGNAL_NAMES: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

/// Register signal name in registration order (must precede `create_signal` for that slot).
pub fn register_signal_name(name: &str) {
    SIGNAL_NAMES.with(|names| names.borrow_mut().push(name.to_string()));
}

fn signal_id(name: &str) -> usize {
    SIGNAL_NAMES.with(|names| names.borrow().iter().position(|n| n == name).unwrap_or(0))
}

/// Parse onClick action strings emitted by w3cos-compiler.
pub fn parse_action_string(action_str: &str) -> EventAction {
    let parts: Vec<&str> = action_str.split(':').collect();
    if parts.first().is_some_and(|part| part.trim() == "speech") {
        return match parts.get(1).map(|part| part.trim()) {
            Some("start") if parts.len() >= 10 => EventAction::SpeechRecognitionStart {
                transcript_signal: signal_id(parts[2].trim()),
                final_signal: signal_id(parts[3].trim()),
                confidence_signal: signal_id(parts[4].trim()),
                status_signal: signal_id(parts[5].trim()),
                lang: parts[6].trim().to_string(),
                process_locally: matches!(parts[7].trim(), "1" | "true"),
                continuous: matches!(parts[8].trim(), "1" | "true"),
                interim_results: matches!(parts[9].trim(), "1" | "true"),
            },
            Some("stop") => EventAction::SpeechRecognitionStop {
                after_signal: parts.get(2).map(|name| signal_id(name.trim())),
                after_value: parts
                    .get(3)
                    .and_then(|value| value.trim().parse::<i64>().ok())
                    .unwrap_or(0),
            },
            _ => EventAction::None,
        };
    }
    if parts.len() >= 2 && parts[0].trim() == "history" {
        let op = parts[1].trim();
        let route_name = parts.get(2).map(|s| s.trim()).unwrap_or("route");
        let route_signal = signal_id(route_name);
        return match op {
            "push" => {
                let route_value = parts
                    .get(3)
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .unwrap_or(0);
                let path = parts.get(4..).map(|p| p.join(":")).unwrap_or_default();
                EventAction::HistoryPush {
                    route_signal,
                    route_value,
                    path,
                }
            }
            "back" => EventAction::HistoryBack { route_signal },
            _ => EventAction::None,
        };
    }
    if parts.len() >= 5 && parts[0].trim() == "fetch" {
        let status_signal = signal_id(parts[2].trim());
        let bytes_signal = signal_id(parts[3].trim());
        let url = parts[4..].join(":");
        return EventAction::FetchGet {
            url,
            status_signal,
            bytes_signal,
        };
    }
    if parts.len() < 2 {
        return EventAction::None;
    }
    let op = parts[0].trim();
    let id = signal_id(parts[1].trim());
    match op {
        "increment" => EventAction::Increment(id),
        "decrement" => EventAction::Decrement(id),
        "toggle" => EventAction::Toggle(id),
        "set" => {
            let value = parts
                .get(2)
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(0);
            EventAction::Set(id, value)
        }
        _ => EventAction::None,
    }
}

/// Register reactive signals once. `build_ui` may run many times; signal slots must not grow.
pub fn ensure_signals(init: impl FnOnce()) {
    SIGNALS_REGISTERED.with(|registered| {
        if !*registered.borrow() {
            init();
            *registered.borrow_mut() = true;
        }
    });
}

/// Create a new signal with the given initial value. Returns the signal ID.
pub fn create_signal(initial: i64) -> usize {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        let id = store.values.len();
        store.values.push(initial);
        id
    })
}

/// Create a string-valued reactive signal. Numeric reads return zero, while
/// generated text interpolation uses [`get_signal_text`].
pub fn create_text_signal(initial: &str) -> usize {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        let id = store.values.len();
        store.values.push(0);
        store.text_values.insert(id, initial.to_string());
        id
    })
}

/// Read the current value of a signal.
pub fn get_signal(id: usize) -> i64 {
    STORE.with(|s| s.borrow().values.get(id).copied().unwrap_or(0))
}

/// Read a signal in display form, preserving string-valued signals.
pub fn get_signal_text(id: usize) -> String {
    STORE.with(|s| {
        let store = s.borrow();
        store
            .text_values
            .get(&id)
            .cloned()
            .unwrap_or_else(|| store.values.get(id).copied().unwrap_or(0).to_string())
    })
}

/// Set a signal to a new value. Marks the store as dirty.
pub fn set_signal(id: usize, value: i64) {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        if let Some(slot) = store.values.get_mut(id)
            && *slot != value
        {
            *slot = value;
            store.dirty = true;
        }
    })
}

/// Update a string-valued signal and mark the component tree dirty.
pub fn set_text_signal(id: usize, value: impl Into<String>) {
    let value = value.into();
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        let changed = store.text_values.get(&id) != Some(&value);
        if changed {
            store.text_values.insert(id, value);
            store.dirty = true;
        }
    })
}

/// Check if any signal has been modified since the last `clear_dirty()`.
pub fn is_dirty() -> bool {
    STORE.with(|s| s.borrow().dirty)
}

/// Clear the dirty flag after a UI rebuild.
pub fn clear_dirty() {
    STORE.with(|s| s.borrow_mut().dirty = false)
}

/// Mark the component tree dirty without changing a signal.
/// The shell compositor uses this while a window is being dragged.
pub fn mark_dirty() {
    STORE.with(|s| s.borrow_mut().dirty = true);
}

thread_local! {
    static SHELL_VIEWPORT: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

fn configured_viewport() -> (f32, f32) {
    if let Ok(raw) = std::env::var("W3COS_VIEW") {
        let mut parts = raw.split(['x', 'X', ',']);
        if let (Some(width), Some(height)) = (parts.next(), parts.next())
            && let (Ok(width), Ok(height)) =
                (width.trim().parse::<f32>(), height.trim().parse::<f32>())
            && width >= 200.0
            && height >= 200.0
        {
            return (width, height);
        }
    }
    (1200.0, 800.0)
}

/// Logical window size the shell should lay out against.
/// `W3COS_VIEW=390x760` overrides the 1200×800 desktop default.
pub fn shell_viewport() -> (f32, f32) {
    SHELL_VIEWPORT.with(|slot| {
        let current = slot.get();
        if current.0 < 1.0 {
            let initial = configured_viewport();
            slot.set(initial);
            initial
        } else {
            current
        }
    })
}

/// Remember the platform viewport. Returns true when the size changed so the
/// shell can rebuild its desktop, tablet, or phone posture.
pub fn note_shell_viewport(width: f32, height: f32) -> bool {
    let width = width.round();
    let height = height.round();
    if width < 1.0 || height < 1.0 {
        return false;
    }
    let (current_w, current_h) = shell_viewport();
    if (current_w - width).abs() < 0.5 && (current_h - height).abs() < 0.5 {
        return false;
    }
    SHELL_VIEWPORT.with(|slot| slot.set((width, height)));
    true
}

/// Focused text field the shell IME may edit. The runtime notes it before
/// forwarding a key. `role` is the `--w3cos-ime` marker.
#[derive(Clone, Debug, PartialEq)]
pub struct ImeFieldNote {
    pub index: usize,
    pub value: String,
    pub role: String,
    pub secure: bool,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

thread_local! {
    static POINTER_COARSE: Cell<Option<bool>> = const { Cell::new(None) };
    static PRESS_ORIGIN: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    static SWIPE_HOOK: Cell<Option<fn(f32, f32, f32, f32) -> bool>> = const { Cell::new(None) };
    static KEY_HOOK: Cell<Option<fn(&str) -> bool>> = const { Cell::new(None) };
    static FOCUS_PLACEHOLDER: RefCell<Option<String>> = const { RefCell::new(None) };
    static IME_FIELD: RefCell<Option<ImeFieldNote>> = const { RefCell::new(None) };
    static IME_REPLACEMENTS: RefCell<Vec<(usize, String)>> = const { RefCell::new(Vec::new()) };
}

/// `W3COS_POINTER=coarse` or `fine` overrides the last pointer sample.
/// Touch and pen count as coarse. Mouse counts as fine. Unknown stays fine.
pub fn pointer_is_coarse() -> bool {
    match std::env::var("W3COS_POINTER").ok().as_deref() {
        Some("coarse") => true,
        Some("fine") => false,
        _ => POINTER_COARSE.with(|slot| slot.get().unwrap_or(false)),
    }
}

/// Record a pointer sample. Returns true when coarse/fine changed.
pub fn note_pointer_coarse(coarse: bool) -> bool {
    POINTER_COARSE.with(|slot| {
        if slot.get() == Some(coarse) {
            return false;
        }
        slot.set(Some(coarse));
        true
    })
}

/// Remember where this press started, and whether the pointer is coarse.
pub fn note_pointer_sample(kind: &str, x: f32, y: f32) {
    let coarse = matches!(kind, "touch" | "pen");
    if note_pointer_coarse(coarse) {
        mark_dirty();
    }
    PRESS_ORIGIN.with(|slot| slot.set(Some((x, y))));
}

pub fn set_shell_swipe_hook(hook: fn(f32, f32, f32, f32) -> bool) {
    SWIPE_HOOK.with(|slot| slot.set(Some(hook)));
}

/// Deliver a completed press if it moved. The hook returns true when the
/// shell consumed the gesture and the runtime must not also click.
pub fn take_shell_swipe(x: f32, y: f32) -> bool {
    let Some((x0, y0)) = PRESS_ORIGIN.with(|slot| slot.take()) else {
        return false;
    };
    SWIPE_HOOK.with(|slot| slot.get().is_some_and(|hook| hook(x0, y0, x - x0, y - y0)))
}

pub fn set_shell_key_hook(hook: fn(&str) -> bool) {
    KEY_HOOK.with(|slot| slot.set(Some(hook)));
}

pub fn shell_key(name: &str) -> bool {
    KEY_HOOK.with(|slot| slot.get().is_some_and(|hook| hook(name)))
}

pub fn note_ime_field(note: ImeFieldNote) {
    IME_FIELD.with(|slot| *slot.borrow_mut() = Some(note));
}

pub fn current_ime_field() -> Option<ImeFieldNote> {
    IME_FIELD.with(|slot| slot.borrow().clone())
}

pub fn clear_ime_field() {
    IME_FIELD.with(|slot| *slot.borrow_mut() = None);
}

/// Queue a full replacement for a text-input flat index. The window applies
/// every queued edit after the shell key hook or a candidate click.
pub fn queue_ime_replacement(index: usize, text: impl Into<String>) {
    let text = text.into();
    IME_REPLACEMENTS.with(|slot| slot.borrow_mut().push((index, text)));
}

pub fn take_ime_replacements() -> Vec<(usize, String)> {
    IME_REPLACEMENTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

pub fn request_focus_placeholder(placeholder: &str) {
    FOCUS_PLACEHOLDER.with(|slot| *slot.borrow_mut() = Some(placeholder.to_string()));
}

pub fn take_focus_placeholder() -> Option<String> {
    FOCUS_PLACEHOLDER.with(|slot| slot.borrow_mut().take())
}

/// All signal values in registration order (for UI tests).
pub fn all_signal_values() -> Vec<i64> {
    STORE.with(|s| s.borrow().values.clone())
}

/// Reset the entire store (used between rebuilds to re-register signals idempotently).
pub fn reset() {
    STORE.with(|s| {
        let mut store = s.borrow_mut();
        store.dirty = false;
    })
}

thread_local! {
    static INVOKE_HOOK: RefCell<Option<fn(u8, &str)>> = const { RefCell::new(None) };
    static AGENT_POLL: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

/// Shell hook drained on the UI thread after the L2 worker finishes.
/// Layout and paint do not call it.
pub fn set_agent_poll_hook(hook: fn()) {
    AGENT_POLL.with(|slot| slot.set(Some(hook)));
}

pub fn run_agent_poll_hook() {
    AGENT_POLL.with(|slot| {
        if let Some(hook) = slot.get() {
            hook();
        }
    });
}

/// Register the shell intent hook. The window calls it from a button click or
/// an Enter submit, not from layout or paint.
pub fn set_invoke_hook(hook: fn(u8, &str)) {
    INVOKE_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

/// `phase`: 0 press, 1 move, 2 release. `dx`/`dy` are from the press point.
/// The hook runs on the UI thread and may mark the tree dirty.
pub type ShellDragHook = fn(id: i64, mode: u8, dx: f32, dy: f32, phase: u8);

struct DragCapture {
    id: i64,
    mode: u8,
    start_x: f32,
    start_y: f32,
    last_dx: f32,
    last_dy: f32,
    moved: bool,
}

thread_local! {
    static DRAG_HOOK: Cell<Option<ShellDragHook>> = const { Cell::new(None) };
    static DRAG_CAPTURE: RefCell<Option<DragCapture>> = const { RefCell::new(None) };
}

pub fn set_shell_drag_hook(hook: ShellDragHook) {
    DRAG_HOOK.with(|slot| slot.set(Some(hook)));
}

pub fn shell_drag_clear() {
    DRAG_CAPTURE.with(|slot| *slot.borrow_mut() = None);
}

pub fn shell_drag_begin(id: i64, mode: u8, x: f32, y: f32) {
    DRAG_CAPTURE.with(|slot| {
        *slot.borrow_mut() = Some(DragCapture {
            id,
            mode,
            start_x: x,
            start_y: y,
            last_dx: 0.0,
            last_dy: 0.0,
            moved: false,
        });
    });
    if let Some(hook) = DRAG_HOOK.with(Cell::get) {
        hook(id, mode, 0.0, 0.0, 0);
    }
}

pub fn shell_drag_move(x: f32, y: f32) {
    let event = DRAG_CAPTURE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let cap = slot.as_mut()?;
        let dx = x - cap.start_x;
        let dy = y - cap.start_y;
        if !cap.moved && dx.abs() < 4.0 && dy.abs() < 4.0 {
            return None;
        }
        cap.moved = true;
        cap.last_dx = dx;
        cap.last_dy = dy;
        Some((cap.id, cap.mode, dx, dy))
    });
    if let Some((id, mode, dx, dy)) = event
        && let Some(hook) = DRAG_HOOK.with(Cell::get)
    {
        hook(id, mode, dx, dy, 1);
    }
}

pub fn shell_drag_is_active() -> bool {
    DRAG_CAPTURE.with(|slot| slot.borrow().is_some())
}

/// Returns true when the pointer moved far enough that the click should not run.
pub fn shell_drag_end() -> bool {
    let finished = DRAG_CAPTURE.with(|slot| slot.borrow_mut().take());
    let Some(cap) = finished else {
        return false;
    };
    if !cap.moved {
        return false;
    }
    if let Some(hook) = DRAG_HOOK.with(Cell::get) {
        hook(cap.id, cap.mode, cap.last_dx, cap.last_dy, 2);
    }
    true
}

fn node_at(root: &Component, target: usize) -> Option<&Component> {
    fn walk<'a>(comp: &'a Component, target: usize, counter: &mut usize) -> Option<&'a Component> {
        let index = *counter;
        *counter += 1;
        if index == target {
            return Some(comp);
        }
        for child in &comp.children {
            if let Some(found) = walk(child, target, counter) {
                return Some(found);
            }
        }
        None
    }
    walk(root, target, &mut 0)
}

pub fn shell_win_label(root: &Component, index: usize) -> Option<String> {
    node_at(root, index)?
        .style
        .custom_properties
        .as_ref()?
        .get("--w3cos-win")
        .cloned()
}

pub fn with_shell_win_draft(root: &Component, index: usize, draft: String) -> String {
    if !draft.is_empty() {
        return draft;
    }
    if let Some(label) = node_at(root, index)
        .and_then(|node| node.style.custom_properties.as_ref())
        .and_then(|props| props.get("--w3cos-draft"))
        .cloned()
    {
        return label;
    }
    shell_win_label(root, index).unwrap_or(draft)
}

/// Title-bar and resize-edge gesture on this node. `mode` 0 moves; 1–8 resize.
pub fn shell_gesture_at(root: &Component, index: usize) -> Option<(i64, u8)> {
    let props = node_at(root, index)?.style.custom_properties.as_ref()?;
    let id = props.get("--w3cos-win")?.parse().ok()?;
    let mode = match props.get("--w3cos-gesture")?.as_str() {
        "move" => 0,
        "n" => 1,
        "s" => 2,
        "e" => 3,
        "w" => 4,
        "ne" => 5,
        "nw" => 6,
        "se" => 7,
        "sw" => 8,
        _ => return None,
    };
    Some((id, mode))
}

/// Drop a hook installed by [`set_invoke_hook`]. Tests use this so a plugin
/// callback does not stay registered for later cases on the same thread.
pub fn clear_invoke_hook() {
    INVOKE_HOOK.with(|slot| *slot.borrow_mut() = None);
}

/// Execute an event action against the signal store.
pub fn execute_action(action: &w3cos_std::EventAction) {
    execute_action_with_draft(action, "");
}

/// Like [`execute_action`], but `draft` is the intent-field text for
/// [`EventAction::Invoke`].
pub fn execute_action_with_draft(action: &w3cos_std::EventAction, draft: &str) {
    match action {
        w3cos_std::EventAction::None => {}
        w3cos_std::EventAction::Increment(id) => {
            let val = get_signal(*id);
            set_signal(*id, val + 1);
        }
        w3cos_std::EventAction::Decrement(id) => {
            let val = get_signal(*id);
            set_signal(*id, val - 1);
        }
        w3cos_std::EventAction::Set(id, value) => {
            set_signal(*id, *value);
        }
        w3cos_std::EventAction::Toggle(id) => {
            let val = get_signal(*id);
            set_signal(*id, if val == 0 { 1 } else { 0 });
        }
        w3cos_std::EventAction::Notify(title, body) => {
            #[cfg(all(
                any(target_os = "macos", target_os = "linux", target_os = "windows"),
                not(target_env = "ohos")
            ))]
            crate::notification::show(title, body);
            #[cfg(not(all(
                any(target_os = "macos", target_os = "linux", target_os = "windows"),
                not(target_env = "ohos")
            )))]
            {
                let _ = (title, body);
            }
        }
        w3cos_std::EventAction::HistoryPush {
            route_signal,
            route_value,
            path,
        } => {
            set_signal(*route_signal, *route_value);
            let state = route_value.to_string();
            crate::history::push_state(Some(&state), "", path);
        }
        w3cos_std::EventAction::HistoryBack { route_signal } => {
            crate::history::back();
            let restored = crate::history::get_state()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            set_signal(*route_signal, restored);
        }
        w3cos_std::EventAction::FetchGet {
            url,
            status_signal,
            bytes_signal,
        } => {
            let resp = crate::fetch::fetch(url, Default::default());
            set_signal(*status_signal, resp.status as i64);
            let bytes = resp.array_buffer().map(|b| b.len() as i64).unwrap_or(-1);
            set_signal(*bytes_signal, bytes);
        }
        w3cos_std::EventAction::SpeechRecognitionStart {
            transcript_signal,
            final_signal,
            confidence_signal,
            status_signal,
            lang,
            process_locally,
            continuous,
            interim_results,
        } => {
            crate::speech::start(crate::speech::SpeechRecognitionBinding {
                transcript_signal: *transcript_signal,
                final_signal: *final_signal,
                confidence_signal: *confidence_signal,
                status_signal: *status_signal,
                options: crate::speech::SpeechRecognitionOptions {
                    lang: lang.clone(),
                    process_locally: *process_locally,
                    continuous: *continuous,
                    interim_results: *interim_results,
                },
            });
        }
        w3cos_std::EventAction::SpeechRecognitionStop {
            after_signal,
            after_value,
        } => {
            crate::speech::stop();
            if let Some(signal) = after_signal {
                set_signal(*signal, *after_value);
            }
        }
        w3cos_std::EventAction::NativeHost {
            id: host_id, click, ..
        } => {
            if *click {
                host_runtime::dispatch_click(*host_id);
            }
        }
        w3cos_std::EventAction::Invoke { code, .. } => {
            INVOKE_HOOK.with(|slot| {
                if let Some(hook) = *slot.borrow() {
                    hook(*code, draft);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read_signal() {
        let id = create_signal(42);
        assert_eq!(get_signal(id), 42);
    }

    #[test]
    fn set_signal_marks_dirty() {
        let id = create_signal(0);
        clear_dirty();
        assert!(!is_dirty());
        set_signal(id, 10);
        assert!(is_dirty());
        assert_eq!(get_signal(id), 10);
    }

    #[test]
    fn set_same_value_not_dirty() {
        let id = create_signal(5);
        clear_dirty();
        set_signal(id, 5);
        assert!(!is_dirty());
    }

    #[test]
    fn text_signal_preserves_utf8_and_marks_dirty() {
        let id = create_text_signal("等待识别");
        assert_eq!(get_signal_text(id), "等待识别");
        clear_dirty();
        set_text_signal(id, "上海到杭州");
        assert!(is_dirty());
        assert_eq!(get_signal_text(id), "上海到杭州");
    }

    #[test]
    fn execute_increment_action() {
        let id = create_signal(0);
        clear_dirty();
        execute_action(&w3cos_std::EventAction::Increment(id));
        assert_eq!(get_signal(id), 1);
        assert!(is_dirty());
    }

    #[test]
    fn execute_decrement_action() {
        let id = create_signal(10);
        clear_dirty();
        execute_action(&w3cos_std::EventAction::Decrement(id));
        assert_eq!(get_signal(id), 9);
    }

    #[test]
    fn parse_increment_by_signal_name() {
        register_signal_name("taps");
        let id = create_signal(0);
        assert_eq!(id, 0);
        clear_dirty();
        execute_action(&parse_action_string("increment:taps"));
        assert_eq!(get_signal(id), 1);
    }

    #[test]
    fn parse_history_push() {
        register_signal_name("route");
        let _ = create_signal(0);
        execute_action(&parse_action_string("history:push:route:2:/css"));
        assert_eq!(get_signal(0), 2);
    }

    thread_local! {
        static SEEN_INVOKE: RefCell<Option<(u8, String)>> = const { RefCell::new(None) };
    }

    fn record_invoke(code: u8, draft: &str) {
        SEEN_INVOKE.with(|slot| *slot.borrow_mut() = Some((code, draft.to_string())));
    }

    #[test]
    fn invoke_hook_receives_the_draft_without_running_during_reads() {
        set_invoke_hook(record_invoke);
        execute_action_with_draft(
            &w3cos_std::EventAction::Invoke {
                code: 1,
                input_placeholder: String::new(),
            },
            "open files",
        );
        let seen = SEEN_INVOKE.with(|slot| slot.borrow().clone());
        assert_eq!(seen, Some((1, "open files".to_string())));
    }

    #[test]
    fn parse_speech_recognition_start() {
        for name in ["transcript", "final", "confidence", "status"] {
            register_signal_name(name);
            let _ = create_signal(0);
        }
        let action =
            parse_action_string("speech:start:transcript:final:confidence:status:zh-CN:1:1:1");
        assert!(matches!(
            action,
            EventAction::SpeechRecognitionStart {
                process_locally: true,
                continuous: true,
                interim_results: true,
                ..
            }
        ));
    }

    #[test]
    fn shell_gesture_reads_the_hit_node_only() {
        let mut title = Component::row(w3cos_std::Style::default(), vec![]);
        title.style.custom_properties = Some(HashMap::from([
            ("--w3cos-win".to_string(), "2".to_string()),
            ("--w3cos-gesture".to_string(), "move".to_string()),
        ]));
        let root = Component::column(w3cos_std::Style::default(), vec![title]);
        assert_eq!(shell_gesture_at(&root, 1), Some((2, 0)));
        assert_eq!(shell_gesture_at(&root, 0), None);
        assert_eq!(shell_win_label(&root, 1).as_deref(), Some("2"));
        assert_eq!(
            with_shell_win_draft(&root, 1, String::new()),
            "2".to_string()
        );
        assert_eq!(
            with_shell_win_draft(&root, 1, "keep".to_string()),
            "keep".to_string()
        );
    }
}
