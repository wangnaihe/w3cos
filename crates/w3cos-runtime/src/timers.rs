use std::cell::RefCell;
use std::time::{Duration, Instant};

use w3cos_std::EventAction;

#[derive(Debug)]
struct TimerEntry {
    id: u32,
    action: EventAction,
    fire_at: Instant,
    interval: Option<Duration>,
}

struct TimerStore {
    timers: Vec<TimerEntry>,
    next_id: u32,
    raf_actions: Vec<EventAction>,
}

impl TimerStore {
    fn new() -> Self {
        Self {
            timers: Vec::new(),
            next_id: 1,
            raf_actions: Vec::new(),
        }
    }
}

thread_local! {
    static TIMERS: RefCell<TimerStore> = RefCell::new(TimerStore::new());
}

/// Register a one-shot timer. Returns the timer ID.
pub fn set_timeout(action: EventAction, delay_ms: u64) -> u32 {
    TIMERS.with(|t| {
        let mut store = t.borrow_mut();
        let id = store.next_id;
        store.next_id += 1;
        store.timers.push(TimerEntry {
            id,
            action,
            fire_at: Instant::now() + Duration::from_millis(delay_ms),
            interval: None,
        });
        id
    })
}

/// Register a repeating timer. Returns the timer ID.
pub fn set_interval(action: EventAction, interval_ms: u64) -> u32 {
    TIMERS.with(|t| {
        let mut store = t.borrow_mut();
        let id = store.next_id;
        store.next_id += 1;
        let interval = Duration::from_millis(interval_ms);
        store.timers.push(TimerEntry {
            id,
            action,
            fire_at: Instant::now() + interval,
            interval: Some(interval),
        });
        id
    })
}

/// Cancel a timer (works for both timeout and interval).
pub fn clear_timer(id: u32) {
    TIMERS.with(|t| {
        t.borrow_mut().timers.retain(|entry| entry.id != id);
    })
}

/// Register a callback to run before the next repaint.
pub fn request_animation_frame(action: EventAction) {
    TIMERS.with(|t| {
        t.borrow_mut().raf_actions.push(action);
    })
}

/// Fire all due timers and rAF callbacks. Returns actions to execute.
pub fn tick() -> Vec<EventAction> {
    TIMERS.with(|t| {
        let mut store = t.borrow_mut();
        let now = Instant::now();
        let mut actions = Vec::new();

        // Collect due timers
        let mut reschedule = Vec::new();
        store.timers.retain(|entry| {
            if now >= entry.fire_at {
                actions.push(entry.action.clone());
                if let Some(interval) = entry.interval {
                    reschedule.push(TimerEntry {
                        id: entry.id,
                        action: entry.action.clone(),
                        fire_at: now + interval,
                        interval: Some(interval),
                    });
                }
                false
            } else {
                true
            }
        });
        store.timers.extend(reschedule);

        // Drain rAF callbacks
        actions.extend(store.raf_actions.drain(..));

        actions
    })
}

/// Returns the duration until the next timer fires, or None if no timers are pending.
pub fn next_deadline() -> Option<Instant> {
    TIMERS.with(|t| {
        let store = t.borrow();
        let timer_deadline = store.timers.iter().map(|e| e.fire_at).min();
        if !store.raf_actions.is_empty() {
            let raf_deadline = Instant::now();
            match timer_deadline {
                Some(td) => Some(td.min(raf_deadline)),
                None => Some(raf_deadline),
            }
        } else {
            timer_deadline
        }
    })
}

/// Check if there are any pending timers or rAF callbacks.
pub fn has_pending() -> bool {
    TIMERS.with(|t| {
        let store = t.borrow();
        !store.timers.is_empty() || !store.raf_actions.is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_timeout_fires() {
        let id = set_timeout(EventAction::Increment(0), 0);
        assert!(id > 0);
        std::thread::sleep(Duration::from_millis(5));
        let actions = tick();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], EventAction::Increment(0)));
        let actions2 = tick();
        assert!(actions2.is_empty());
    }

    #[test]
    fn set_interval_repeats() {
        let _id = set_interval(EventAction::Increment(0), 1);
        std::thread::sleep(Duration::from_millis(5));
        let actions = tick();
        assert!(!actions.is_empty());
        std::thread::sleep(Duration::from_millis(5));
        let actions2 = tick();
        assert!(!actions2.is_empty());
        clear_timer(_id);
        std::thread::sleep(Duration::from_millis(5));
        let actions3 = tick();
        assert!(actions3.is_empty());
    }

    #[test]
    fn clear_timeout_cancels() {
        let id = set_timeout(EventAction::Increment(0), 1000);
        clear_timer(id);
        std::thread::sleep(Duration::from_millis(5));
        let actions = tick();
        assert!(actions.is_empty());
    }

    #[test]
    fn request_animation_frame_fires_once() {
        request_animation_frame(EventAction::Increment(0));
        let actions = tick();
        assert_eq!(actions.len(), 1);
        let actions2 = tick();
        assert!(actions2.is_empty());
    }

    #[test]
    fn next_deadline_reflects_timers() {
        assert!(next_deadline().is_none());
        let id = set_timeout(EventAction::None, 100);
        assert!(next_deadline().is_some());
        clear_timer(id);
    }
}
