//! File watching for `@parcel/watcher` / `@w3cos/watcher` (M3 item 20).
//!
//! The parcel API (subscribe, ignore lists, event coalescing, snapshots) is
//! glue in `w3cos-node/js/watcher.js`; this is the host half on
//! `__w3cos_node_host`, a recursive [`FileSystemObserver`] per
//! subscription, drained by the glue's poll loop.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

use crate::fs_watch::{ChangeType, FileSystemObserver, ObserveOptions};

thread_local! {
    static OBSERVERS: RefCell<HashMap<i64, FileSystemObserver>> = RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<i64> = const { std::cell::Cell::new(1) };
}

pub(crate) fn install(host: &Value) {
    use w3cos_core::node_host::define;
    // watcherSubscribe(dir) -> id, or a string error message.
    define(host, "watcherSubscribe", |args| {
        let dir = args.first().map(Value::to_js_string).unwrap_or_default();
        if !std::path::Path::new(&dir).is_dir() {
            return Value::from("Bad file descriptor");
        }
        let observer = FileSystemObserver::new();
        observer.observe(&dir, ObserveOptions { recursive: true });
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        OBSERVERS.with(|observers| observers.borrow_mut().insert(id, observer));
        Value::Number(id as f64)
    });
    // watcherPoll(id) -> [[type, path, isDirectory], ...]
    define(host, "watcherPoll", |args| {
        let id = args.first().and_then(Value::as_number).unwrap_or(0.0) as i64;
        let records = OBSERVERS.with(|observers| {
            observers
                .borrow()
                .get(&id)
                .map(FileSystemObserver::poll_records)
                .unwrap_or_default()
        });
        let mut events = Vec::new();
        for record in records {
            let path = |p: &std::path::Path| Value::from(p.to_string_lossy().into_owned());
            let event = |kind: &str, p: &std::path::Path| {
                Value::array(vec![
                    Value::from(kind),
                    path(p),
                    Value::Bool(record.is_directory),
                ])
            };
            match record.change_type {
                ChangeType::Created => events.push(event("create", &record.path)),
                ChangeType::Modified | ChangeType::Unknown => {
                    events.push(event("update", &record.path))
                }
                ChangeType::Deleted => events.push(event("delete", &record.path)),
                ChangeType::Moved => {
                    events.push(event("delete", &record.path));
                    if let Some(to) = &record.moved_to {
                        events.push(event("create", to));
                    }
                }
            }
        }
        Value::array(events)
    });
    define(host, "watcherUnsubscribe", |args| {
        let id = args.first().and_then(Value::as_number).unwrap_or(0.0) as i64;
        if let Some(observer) = OBSERVERS.with(|observers| observers.borrow_mut().remove(&id)) {
            observer.disconnect();
        }
        Value::Undefined
    });
}
