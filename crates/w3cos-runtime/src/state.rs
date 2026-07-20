use std::cell::RefCell;
use std::collections::HashMap;
use w3cos_std::EventAction;

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

/// Execute an event action against the signal store.
pub fn execute_action(action: &w3cos_std::EventAction) {
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
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            crate::notification::show(title, body);
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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
                w3cos_react_compat::aot::dispatch_click(*host_id);
            }
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
}
