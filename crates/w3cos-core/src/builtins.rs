#![allow(non_upper_case_globals, non_snake_case)]

use crate::Value;
use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Copy)]
enum BuiltinKind {
    Math,
    Object,
    Array,
    Console,
    Document,
}

#[derive(Clone, Copy)]
pub struct BuiltinObject(BuiltinKind);

pub const Math: BuiltinObject = BuiltinObject(BuiltinKind::Math);
pub const Object: BuiltinObject = BuiltinObject(BuiltinKind::Object);
pub const Array: BuiltinObject = BuiltinObject(BuiltinKind::Array);
pub const console: BuiltinObject = BuiltinObject(BuiltinKind::Console);
pub const document: BuiltinObject = BuiltinObject(BuiltinKind::Document);

impl BuiltinObject {
    pub fn call_method(&self, key: &str, arguments: Vec<Value>) -> Value {
        match (self.0, key) {
            (BuiltinKind::Math, "min") => arguments
                .into_iter()
                .min_by(|left, right| left.to_number().total_cmp(&right.to_number()))
                .unwrap_or(Value::Number(f64::INFINITY)),
            (BuiltinKind::Math, "max") => arguments
                .into_iter()
                .max_by(|left, right| left.to_number().total_cmp(&right.to_number()))
                .unwrap_or(Value::Number(f64::NEG_INFINITY)),
            (BuiltinKind::Math, "floor") => unary_number(arguments, f64::floor),
            (BuiltinKind::Math, "ceil") => unary_number(arguments, f64::ceil),
            (BuiltinKind::Math, "round") => unary_number(arguments, f64::round),
            (BuiltinKind::Math, "trunc") => unary_number(arguments, f64::trunc),
            (BuiltinKind::Math, "abs") => unary_number(arguments, f64::abs),
            (BuiltinKind::Object, "is") => Value::Bool(
                arguments
                    .first()
                    .zip(arguments.get(1))
                    .is_some_and(|(left, right)| left.strict_eq(right)),
            ),
            (BuiltinKind::Object, "keys") => arguments
                .first()
                .map(object_keys)
                .unwrap_or_else(|| Value::array(Vec::new())),
            (BuiltinKind::Object, "values") => arguments
                .first()
                .map(object_values)
                .unwrap_or_else(|| Value::array(Vec::new())),
            (BuiltinKind::Array, "from") => arguments
                .first()
                .cloned()
                .unwrap_or_else(|| Value::array(Vec::new())),
            (BuiltinKind::Console, _) => Value::Undefined,
            (BuiltinKind::Document, "createElement") => dom_element(),
            _ => Value::Undefined,
        }
    }

    pub fn get_property(&self, key: &str) -> Value {
        match (self.0, key) {
            (BuiltinKind::Document, "body") => dom_element(),
            _ => Value::Undefined,
        }
    }
}

fn unary_number(arguments: Vec<Value>, operation: fn(f64) -> f64) -> Value {
    Value::Number(operation(
        arguments.first().map(Value::to_number).unwrap_or(f64::NAN),
    ))
}

fn object_keys(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::array(
            object
                .borrow()
                .keys()
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
        _ => Value::array(Vec::new()),
    }
}

fn object_values(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let object = object.borrow();
            Value::array(
                object
                    .keys()
                    .into_iter()
                    .map(|key| object.get_direct(&key))
                    .collect(),
            )
        }
        _ => Value::array(Vec::new()),
    }
}

fn dom_element() -> Value {
    let element = Value::object(HashMap::new());
    element.set_property("style", Value::object(HashMap::new()));
    for method in [
        "appendChild",
        "removeChild",
        "observe",
        "unobserve",
        "addEventListener",
        "removeEventListener",
        "hasAttribute",
        "getAttribute",
    ] {
        element.set_property(method, Value::function(|_, _| Value::Undefined));
    }
    element
}

pub fn parseInt(arguments: Vec<Value>) -> Value {
    let value = arguments.first().cloned().unwrap_or(Value::Undefined);
    let parsed = value
        .to_js_string()
        .trim()
        .split(|character: char| !character.is_ascii_digit() && character != '-')
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(f64::NAN);
    Value::Number(parsed)
}

pub fn parseFloat(arguments: Vec<Value>) -> Value {
    let value = arguments.first().cloned().unwrap_or(Value::Undefined);
    let text = value.to_js_string();
    let prefix: String = text
        .chars()
        .take_while(|character| {
            character.is_ascii_digit() || matches!(character, '-' | '+' | '.' | 'e' | 'E')
        })
        .collect();
    Value::Number(prefix.parse::<f64>().unwrap_or(f64::NAN))
}

pub struct Error(pub Value);

impl Error {
    pub fn new(arguments: Vec<Value>) -> Self {
        Self(arguments.first().cloned().unwrap_or(Value::Undefined))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pub fn RangeError(arguments: Vec<Value>) -> Value {
    arguments.first().cloned().unwrap_or(Value::Undefined)
}

pub fn ErrorValue(arguments: Vec<Value>) -> Value {
    arguments.first().cloned().unwrap_or(Value::Undefined)
}

pub struct Map;

impl Map {
    pub fn new(arguments: Vec<Value>) -> Value {
        let mut initial = HashMap::<String, Value>::new();
        let iterable = arguments.first().cloned().unwrap_or(Value::Undefined);
        let entries = iterable.get_property("__w3cosMapEntries");
        let source = if matches!(entries, Value::Array(_)) {
            entries
        } else {
            iterable
        };
        for entry in source.iter() {
            if let Value::Array(pair) = entry {
                let pair = pair.borrow();
                if let Some(key) = pair.first() {
                    initial.insert(
                        key.to_js_string(),
                        pair.get(1).cloned().unwrap_or(Value::Undefined),
                    );
                }
            }
        }
        let values = std::rc::Rc::new(std::cell::RefCell::new(initial));
        let map = Value::object(HashMap::new());
        {
            let values = values.clone();
            map.set_property(
                "get",
                Value::function(move |_, arguments| {
                    let key = arguments
                        .first()
                        .map(Value::to_js_string)
                        .unwrap_or_default();
                    values
                        .borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or(Value::Undefined)
                }),
            );
        }
        {
            let values = values.clone();
            let map_reference = map.clone();
            map.set_property(
                "set",
                Value::function(move |_, arguments| {
                    let key = arguments
                        .first()
                        .map(Value::to_js_string)
                        .unwrap_or_default();
                    let value = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                    values.borrow_mut().insert(key, value);
                    sync_map_iteration_properties(&map_reference, &values.borrow());
                    map_reference.clone()
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "has",
                Value::function(move |_, arguments| {
                    let key = arguments
                        .first()
                        .map(Value::to_js_string)
                        .unwrap_or_default();
                    Value::Bool(values.borrow().contains_key(&key))
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "forEach",
                Value::function(move |_, arguments| {
                    let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
                    for (key, value) in values.borrow().iter() {
                        callback.call(
                            Value::Undefined,
                            vec![value.clone(), Value::from(key.clone())],
                        );
                    }
                    Value::Undefined
                }),
            );
        }
        sync_map_iteration_properties(&map, &values.borrow());
        map
    }
}

fn sync_map_iteration_properties(map: &Value, values: &HashMap<String, Value>) {
    let entries = values
        .iter()
        .map(|(key, value)| Value::array(vec![Value::from(key.clone()), value.clone()]))
        .collect::<Vec<_>>();
    let map_values = values.values().cloned().collect::<Vec<_>>();
    map.set_property("size", Value::Number(values.len() as f64));
    map.set_property("__w3cosMapEntries", Value::array(entries));
    map.set_property("__w3cosMapValues", Value::array(map_values));
}

pub struct ResizeObserver {
    _private: (),
}

pub const ResizeObserver: Value = Value::Undefined;

struct ResizeObserverTarget {
    element: Value,
    last_size: Option<(f32, f32)>,
}

struct ResizeObserverState {
    callback: Value,
    targets: std::collections::HashMap<u64, ResizeObserverTarget>,
}

thread_local! {
    static NEXT_RESIZE_OBSERVER: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static RESIZE_OBSERVERS: std::cell::RefCell<std::collections::HashMap<u64, ResizeObserverState>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

impl ResizeObserver {
    pub fn new(arguments: Vec<Value>) -> Value {
        let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
        let observer_id = NEXT_RESIZE_OBSERVER.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        RESIZE_OBSERVERS.with(|observers| {
            observers.borrow_mut().insert(
                observer_id,
                ResizeObserverState {
                    callback: callback.clone(),
                    targets: std::collections::HashMap::new(),
                },
            );
        });

        let observer = dom_element();
        let observe_callback = callback;
        observer.set_property(
            "observe",
            Value::function(move |_, arguments| {
                let element = arguments.first().cloned().unwrap_or(Value::Undefined);
                let host_id = element
                    .get_property("__w3cosHostId")
                    .to_js_string()
                    .parse::<u64>()
                    .ok();
                if let Some(host_id) = host_id {
                    if std::env::var_os("W3COS_RESIZE_TRACE").is_some() {
                        eprintln!("[W3C OS][RESIZE] observe host={host_id}");
                    }
                    RESIZE_OBSERVERS.with(|observers| {
                        observers
                            .borrow_mut()
                            .entry(observer_id)
                            .or_insert_with(|| ResizeObserverState {
                                callback: observe_callback.clone(),
                                targets: std::collections::HashMap::new(),
                            })
                            .targets
                            .insert(
                                host_id,
                                ResizeObserverTarget {
                                    element,
                                    last_size: None,
                                },
                            );
                    });
                }
                Value::Undefined
            }),
        );
        observer.set_property(
            "unobserve",
            Value::function(move |_, arguments| {
                let host_id = arguments
                    .first()
                    .map(|element| element.get_property("__w3cosHostId"))
                    .map(|value| value.to_js_string())
                    .and_then(|value| value.parse::<u64>().ok());
                if let Some(host_id) = host_id {
                    RESIZE_OBSERVERS.with(|observers| {
                        if let Some(observer) = observers.borrow_mut().get_mut(&observer_id) {
                            observer.targets.remove(&host_id);
                        }
                    });
                }
                Value::Undefined
            }),
        );
        observer.set_property(
            "disconnect",
            Value::function(move |_, _| {
                RESIZE_OBSERVERS.with(|observers| {
                    observers.borrow_mut().remove(&observer_id);
                });
                Value::Undefined
            }),
        );
        observer
    }
}

/// Deliver native border-box measurements to JavaScript `ResizeObserver`
/// callbacks. Returns `true` when at least one callback was invoked.
pub fn dispatch_resize_observers(sizes: &[(u64, f32, f32)]) -> bool {
    dispatch_resize_observers_bounded(sizes, usize::MAX).0
}

/// Deliver at most `max_entries` changed native border-box measurements.
///
/// The second return value is `true` when the entry budget was exhausted.
/// Callers should schedule another delivery turn in that case; targets which
/// were not delivered deliberately keep their previous size.
pub fn dispatch_resize_observers_bounded(
    sizes: &[(u64, f32, f32)],
    max_entries: usize,
) -> (bool, bool) {
    let sizes: std::collections::HashMap<u64, (f32, f32)> = sizes
        .iter()
        .map(|(host_id, width, height)| (*host_id, (*width, *height)))
        .collect();
    let mut remaining = max_entries.max(1);
    let deliveries = RESIZE_OBSERVERS.with(|observers| {
        let mut observers = observers.borrow_mut();
        let mut deliveries = Vec::new();
        for observer in observers.values_mut() {
            let mut entries = Vec::new();
            let mut host_ids = observer.targets.keys().copied().collect::<Vec<_>>();
            host_ids.sort_unstable();
            for host_id in host_ids {
                if remaining == 0 {
                    break;
                }
                let Some(target) = observer.targets.get_mut(&host_id) else {
                    continue;
                };
                let Some(&(width, height)) = sizes.get(&host_id) else {
                    continue;
                };
                if target.last_size.is_some_and(|(last_width, last_height)| {
                    (last_width - width).abs() <= 0.01 && (last_height - height).abs() <= 0.01
                }) {
                    continue;
                }
                target.last_size = Some((width, height));
                if std::env::var_os("W3COS_RESIZE_TRACE").is_some() {
                    eprintln!("[W3C OS][RESIZE] host={host_id} border-box={width:.2}x{height:.2}");
                }

                let border_box = Value::object(std::collections::HashMap::from([
                    ("inlineSize".into(), Value::Number(width as f64)),
                    ("blockSize".into(), Value::Number(height as f64)),
                ]));
                let content_rect = Value::object(std::collections::HashMap::from([
                    ("x".into(), Value::Number(0.0)),
                    ("y".into(), Value::Number(0.0)),
                    ("width".into(), Value::Number(width as f64)),
                    ("height".into(), Value::Number(height as f64)),
                ]));
                entries.push(Value::object(std::collections::HashMap::from([
                    ("target".into(), target.element.clone()),
                    ("contentRect".into(), content_rect),
                    (
                        "borderBoxSize".into(),
                        Value::array(vec![border_box.clone()]),
                    ),
                    ("contentBoxSize".into(), Value::array(vec![border_box])),
                ])));
                remaining -= 1;
            }
            if !entries.is_empty() && observer.callback.is_function() {
                deliveries.push((observer.callback.clone(), Value::array(entries)));
            }
            if remaining == 0 {
                break;
            }
        }
        deliveries
    });

    let delivered = !deliveries.is_empty();
    for (callback, entries) in deliveries {
        callback.call(Value::Undefined, vec![entries]);
    }
    (delivered, remaining == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_floor_matches_javascript_number_semantics() {
        assert_eq!(
            Math.call_method("floor", vec![Value::Number(2.75)])
                .to_number(),
            2.0
        );
        assert_eq!(
            Math.call_method("floor", vec![Value::Number(-2.25)])
                .to_number(),
            -3.0
        );
        assert!(Math.call_method("floor", vec![]).to_number().is_nan());
    }

    #[test]
    fn map_constructor_copies_entries_and_iterates_values() {
        let first = Map::new(vec![]);
        first.call_method("set", vec![Value::from("24"), Value::Number(106.0)]);
        first.call_method("set", vec![Value::from("25"), Value::Number(82.0)]);

        let copy = Map::new(vec![first]);
        assert_eq!(
            copy.call_method("get", vec![Value::from("24")]).to_number(),
            106.0
        );
        assert_eq!(copy.get_property("size").to_number(), 2.0);
        let mut heights = copy
            .iter()
            .map(|value| value.to_number())
            .collect::<Vec<_>>();
        heights.sort_by(f64::total_cmp);
        assert_eq!(heights, vec![82.0, 106.0]);
    }

    #[test]
    fn resize_observer_delivers_changed_border_box_sizes_once() {
        let deliveries = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = deliveries.clone();
        let observer = ResizeObserver::new(vec![Value::function(move |_, arguments| {
            let entry = arguments[0].get_property("0");
            recorded.borrow_mut().push(
                entry
                    .get_property("borderBoxSize")
                    .get_property("0")
                    .get_property("blockSize")
                    .to_number(),
            );
            Value::Undefined
        })]);
        let target = dom_element();
        target.set_property("__w3cosHostId", Value::from("42"));
        observer.call_method("observe", vec![target.clone()]);

        assert!(dispatch_resize_observers(&[(42, 320.0, 84.0)]));
        assert!(!dispatch_resize_observers(&[(42, 320.0, 84.0)]));
        assert!(dispatch_resize_observers(&[(42, 320.0, 112.0)]));
        observer.call_method("disconnect", vec![]);
        assert!(!dispatch_resize_observers(&[(42, 320.0, 128.0)]));
        observer.call_method("observe", vec![target]);
        assert!(dispatch_resize_observers(&[(42, 320.0, 128.0)]));
        assert_eq!(&*deliveries.borrow(), &[84.0, 112.0, 128.0]);
    }

    #[test]
    fn resize_observer_defers_entries_beyond_delivery_budget() {
        let deliveries = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = deliveries.clone();
        let observer = ResizeObserver::new(vec![Value::function(move |_, arguments| {
            recorded
                .borrow_mut()
                .push(arguments[0].get_property("length").to_number() as usize);
            Value::Undefined
        })]);
        for host_id in 100..106 {
            let target = dom_element();
            target.set_property("__w3cosHostId", Value::from(host_id.to_string()));
            observer.call_method("observe", vec![target]);
        }
        let sizes = (100..106)
            .map(|host_id| (host_id, 320.0, 80.0 + host_id as f32))
            .collect::<Vec<_>>();

        assert_eq!(dispatch_resize_observers_bounded(&sizes, 4), (true, true));
        assert_eq!(dispatch_resize_observers_bounded(&sizes, 4), (true, false));
        assert_eq!(dispatch_resize_observers_bounded(&sizes, 4), (false, false));
        assert_eq!(&*deliveries.borrow(), &[4, 2]);
        observer.call_method("disconnect", vec![]);
    }
}
