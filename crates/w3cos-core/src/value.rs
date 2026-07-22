use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// JavaScript-compatible dynamic value type.
///
/// Maps the full ECMAScript value space into Rust with reference-counted
/// sharing for heap types (Array, Object, Function).
#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<crate::JsObject>>),
    Function(JsFunction),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::Number(left), Value::Number(right)) => left == right,
            (Value::String(left), Value::String(right)) => left == right,
            (Value::Array(left), Value::Array(right)) => Rc::ptr_eq(left, right),
            (Value::Object(left), Value::Object(right)) => Rc::ptr_eq(left, right),
            (Value::Function(_), Value::Function(_)) => false,
            _ => false,
        }
    }
}

/// A callable JS function stored as a reference-counted closure.
#[derive(Clone)]
pub struct JsFunction {
    inner: Rc<dyn Fn(Value, Vec<Value>) -> Value>,
    /// Properties assigned on the function value. JS functions are objects:
    /// `id.toString = () => name` (monaco's service decorators), static
    /// methods installed on constructor functions, etc.
    props: Rc<RefCell<std::collections::HashMap<String, Value>>>,
}

impl JsFunction {
    pub fn new(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        Self {
            inner: Rc::new(f),
            props: Rc::new(RefCell::new(std::collections::HashMap::new())),
        }
    }

    pub fn call(&self, this: Value, args: Vec<Value>) -> Value {
        (self.inner)(this, args)
    }

    /// Read a property of the function object (Undefined when absent).
    pub fn get_property(&self, key: &str) -> Value {
        self.props
            .borrow()
            .get(key)
            .cloned()
            .unwrap_or(Value::Undefined)
    }

    /// Assign a property on the function object.
    pub fn set_property(&self, key: &str, value: Value) {
        self.props.borrow_mut().insert(key.to_string(), value);
    }

    /// A stable identity address for this function value (clones of the same
    /// `JsFunction` share it) — used for identity-keyed collections (JS Map).
    pub fn identity(&self) -> usize {
        Rc::as_ptr(&self.inner) as *const u8 as usize
    }

    /// Function identity: two `JsFunction`s are the same function when they
    /// share the inner closure allocation (clones of one value do).
    pub fn ptr_eq(&self, other: &JsFunction) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for JsFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Function]")
    }
}

// ── Type coercion ──────────────────────────────────────────────────────

impl Value {
    /// Stable identity hash with ECMAScript `Object.is` semantics.
    ///
    /// Heap values use reference identity, while primitives use their value.
    /// This is suitable for React-style hook dependency comparison.
    pub fn identity_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self {
            Value::Undefined => 0_u8.hash(&mut hasher),
            Value::Null => 1_u8.hash(&mut hasher),
            Value::Bool(value) => {
                2_u8.hash(&mut hasher);
                value.hash(&mut hasher);
            }
            Value::Number(value) => {
                3_u8.hash(&mut hasher);
                if value.is_nan() {
                    u64::MAX.hash(&mut hasher);
                } else {
                    value.to_bits().hash(&mut hasher);
                }
            }
            Value::String(value) => {
                4_u8.hash(&mut hasher);
                value.hash(&mut hasher);
            }
            Value::Array(value) => {
                5_u8.hash(&mut hasher);
                (Rc::as_ptr(value) as usize).hash(&mut hasher);
            }
            Value::Object(value) => {
                6_u8.hash(&mut hasher);
                (Rc::as_ptr(value) as usize).hash(&mut hasher);
            }
            Value::Function(value) => {
                7_u8.hash(&mut hasher);
                value.identity().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// ECMAScript `typeof` operator.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) | Value::Object(_) => "object",
            Value::Function(_) => "function",
        }
    }

    /// ECMAScript `ToBoolean`.
    pub fn to_bool(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            Value::Array(_) | Value::Object(_) | Value::Function(_) => true,
        }
    }

    /// ECMAScript `ToNumber`.
    pub fn to_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Number(n) => *n,
            Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }

    /// ECMAScript `ToString`.
    pub fn to_js_string(&self) -> String {
        match self {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => {
                if n.is_nan() {
                    "NaN".into()
                } else if n.is_infinite() {
                    if *n > 0.0 {
                        "Infinity".into()
                    } else {
                        "-Infinity".into()
                    }
                } else if *n == 0.0 {
                    "0".into()
                } else if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                let elems: Vec<String> = arr.borrow().iter().map(|v| v.to_js_string()).collect();
                elems.join(",")
            }
            Value::Object(_) => "[object Object]".into(),
            Value::Function(function) => {
                let to_string = function.get_property("toString");
                if matches!(to_string, Value::Function(_) | Value::Object(_)) {
                    if let Value::String(value) = to_string.call(self.clone(), vec![]) {
                        return value;
                    }
                }
                "function() { [native code] }".into()
            }
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }
    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }
    pub fn is_function(&self) -> bool {
        matches!(self, Value::Function(_))
    }

    /// ECMAScript `ToInt32`.
    pub fn to_i32(&self) -> i32 {
        let n = self.to_number();
        if n.is_nan() || n.is_infinite() || n == 0.0 {
            return 0;
        }
        let i = n.trunc() as i64;
        (i % (1i64 << 32)) as i32
    }

    /// ECMAScript `ToUint32`.
    pub fn to_u32(&self) -> u32 {
        self.to_i32() as u32
    }

    /// ECMAScript `in` operator: `key in obj`.
    pub fn js_in(&self, obj: &Value) -> Value {
        let key = self.to_js_string();
        match obj {
            Value::Object(o) => Value::Bool(o.borrow().has(&key)),
            Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>() {
                    Value::Bool(idx < arr.borrow().len())
                } else {
                    Value::Bool(false)
                }
            }
            _ => Value::Bool(false),
        }
    }

    /// Property access: `obj[key]` or `obj.key`.
    pub fn get_property(&self, key: &str) -> Value {
        match self {
            Value::Object(o) => {
                let value = o.borrow().get(key, self).clone();
                if !value.is_undefined() || !o.borrow().may_have_getter_properties() {
                    value
                } else {
                    let getter = o
                        .borrow()
                        .get(&format!("__w3cos_getter_{key}"), self)
                        .clone();
                    getter.call(self.clone(), vec![])
                }
            }
            Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>() {
                    arr.borrow().get(idx).cloned().unwrap_or(Value::Undefined)
                } else if key == "length" {
                    Value::Number(arr.borrow().len() as f64)
                } else if key == "buffer" {
                    // Typed arrays use the same Rc-backed storage as arrays
                    // in the compact runtime; exposing it as `buffer` lets a
                    // new typed-array view reuse/slice those code units.
                    self.clone()
                } else {
                    Value::Undefined
                }
            }
            Value::String(s) => {
                if let Ok(idx) = key.parse::<usize>() {
                    s.encode_utf16()
                        .nth(idx)
                        .and_then(|unit| String::from_utf16(&[unit]).ok())
                        .map(Value::String)
                        .unwrap_or(Value::Undefined)
                } else if key == "length" {
                    Value::Number(s.encode_utf16().count() as f64)
                } else {
                    Value::Undefined
                }
            }
            // JS functions are objects: read attached properties.
            Value::Function(f) => f.get_property(key),
            _ => Value::Undefined,
        }
    }

    /// ECMAScript object-rest copy used by `{ picked, ...rest }`.
    ///
    /// Only own enumerable string properties are copied; the prototype and
    /// excluded bindings are not carried into the result.
    pub fn object_rest(&self, excluded: &[&str]) -> Value {
        let Value::Object(object) = self else {
            return Value::object(HashMap::new());
        };
        let object = object.borrow();
        let properties = object
            .keys()
            .into_iter()
            .filter(|key| !excluded.contains(&key.as_str()))
            .map(|key| {
                let value = object.get_direct(&key);
                (key, value)
            })
            .collect();
        Value::object(properties)
    }

    /// Property assignment: `obj[key] = value`.
    ///
    /// Mirrors the `__w3cos_getter_` read convention with a setter one: when
    /// the object has no own data property `key` but a `__w3cos_setter_{key}`
    /// function is reachable through the prototype chain, the setter is
    /// invoked with the object as receiver instead of storing directly.
    pub fn set_property(&self, key: &str, value: Value) {
        match self {
            Value::Object(o) => {
                let has_own = o.borrow().properties.contains_key(key);
                if !has_own {
                    let setter = o
                        .borrow()
                        .get(&format!("__w3cos_setter_{key}"), self)
                        .clone();
                    if !setter.is_undefined() {
                        setter.call(self.clone(), vec![value]);
                        return;
                    }
                }
                o.borrow_mut().set(key, value, &Value::Undefined);
            }
            Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>() {
                    let mut a = arr.borrow_mut();
                    if idx >= a.len() {
                        a.resize(idx + 1, Value::Undefined);
                    }
                    a[idx] = value;
                }
            }
            // JS functions are objects: properties attach to the function
            // value (decorator ids, constructor statics).
            Value::Function(f) => f.set_property(key, value),
            _ => {}
        }
    }

    /// Delete an own property and return the JavaScript-style success value.
    pub fn delete_property(&self, key: &str) -> Value {
        let deleted = match self {
            Value::Object(object) => object.borrow_mut().delete(key),
            Value::Array(array) => {
                if let Ok(index) = key.parse::<usize>() {
                    if let Some(slot) = array.borrow_mut().get_mut(index) {
                        *slot = Value::Undefined;
                    }
                }
                true
            }
            Value::Function(function) => function.props.borrow_mut().remove(key).is_some(),
            _ => true,
        };
        Value::Bool(deleted)
    }
}

// ── Constructors ───────────────────────────────────────────────────────

impl Value {
    pub fn from_f64(n: f64) -> Self {
        Value::Number(n)
    }
    pub fn from_bool(b: bool) -> Self {
        Value::Bool(b)
    }
    pub fn string(s: &str) -> Self {
        Value::String(s.to_string())
    }

    pub fn array(items: Vec<Value>) -> Self {
        Value::Array(Rc::new(RefCell::new(items)))
    }

    pub fn object(props: HashMap<String, Value>) -> Self {
        Value::Object(Rc::new(RefCell::new(crate::JsObject::from_map(props))))
    }

    pub fn object_from_parts(parts: Vec<Value>) -> Self {
        let mut properties = HashMap::new();
        for part in parts {
            if let Value::Object(object) = part {
                let object = object.borrow();
                for key in object.keys() {
                    properties.insert(key.clone(), object.get_direct(&key));
                }
            }
        }
        Value::object(properties)
    }

    pub fn function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Self {
        Value::Function(JsFunction::new(f))
    }

    /// A plain object that is also callable (a JS class / constructor object).
    pub fn callable(
        props: HashMap<String, Value>,
        f: impl Fn(Value, Vec<Value>) -> Value + 'static,
    ) -> Self {
        Value::Object(Rc::new(RefCell::new(crate::JsObject::with_call_slot(
            props,
            JsFunction::new(f),
        ))))
    }

    /// Invoke a dynamically lowered JavaScript function value.
    pub fn call(&self, this: Value, args: Vec<Value>) -> Value {
        match self {
            Value::Function(function) => function.call(this, args),
            Value::Object(object) => {
                let slot = object.borrow().call_slot().cloned();
                match slot {
                    Some(function) => function.call(this, args),
                    None => Value::Undefined,
                }
            }
            _ => Value::Undefined,
        }
    }

    /// Invoke a property as a method while preserving the JavaScript receiver.
    pub fn call_method(&self, key: &str, args: Vec<Value>) -> Value {
        if key == "__w3cos_symbol_iterator" {
            return iterator_object(self.iter().collect());
        }
        if let Value::Array(values) = self
            && let Some(result) = array_call_method(values, key, args.clone(), self)
        {
            return result;
        }
        match (self, key) {
            (Value::Function(_), "call") => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                return self.call(this_arg, args.into_iter().skip(1).collect());
            }
            (Value::Function(_), "apply") => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let applied_args = match args.get(1) {
                    Some(Value::Array(values)) => values.borrow().clone(),
                    _ => Vec::new(),
                };
                return self.call(this_arg, applied_args);
            }
            (Value::Function(_), "bind") => {
                let target = self.clone();
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let bound_args: Vec<Value> = args.into_iter().skip(1).collect();
                return Value::function(move |_, call_args| {
                    let mut combined = bound_args.clone();
                    combined.extend(call_args);
                    target.call(this_arg.clone(), combined)
                });
            }
            (Value::String(value), "endsWith") => {
                return Value::Bool(
                    args.first()
                        .is_some_and(|suffix| value.ends_with(&suffix.to_js_string())),
                );
            }
            (Value::String(value), "slice") => {
                let units = value.encode_utf16().collect::<Vec<_>>();
                let length = units.len() as i64;
                let normalize = |argument: Option<&Value>, fallback: i64| {
                    let raw = argument
                        .map(Value::to_number)
                        .filter(|number| number.is_finite())
                        .map(|number| number.trunc() as i64)
                        .unwrap_or(fallback);
                    if raw < 0 {
                        (length + raw).max(0) as usize
                    } else {
                        raw.min(length) as usize
                    }
                };
                let start = normalize(args.first(), 0);
                let end = normalize(args.get(1), length).max(start);
                return Value::String(String::from_utf16_lossy(&units[start..end]));
            }
            (Value::String(value), "substr") => {
                let units = value.encode_utf16().collect::<Vec<_>>();
                let length = units.len() as i64;
                let raw_start = args
                    .first()
                    .map(Value::to_number)
                    .filter(|number| number.is_finite())
                    .map(|number| number.trunc() as i64)
                    .unwrap_or(0);
                let start = if raw_start < 0 {
                    (length + raw_start).max(0)
                } else {
                    raw_start.min(length)
                } as usize;
                let count = args
                    .get(1)
                    .map(Value::to_number)
                    .filter(|number| number.is_finite())
                    .map(|number| number.trunc().max(0.0) as usize)
                    .unwrap_or(units.len() - start);
                let end = start.saturating_add(count).min(units.len());
                return Value::String(String::from_utf16_lossy(&units[start..end]));
            }
            (Value::String(value), "startsWith") => {
                let needle = args.first().cloned().unwrap_or_default().to_js_string();
                let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let start = string_index_to_byte(value, start);
                return Value::Bool(value[start..].starts_with(&needle));
            }
            (Value::String(value), "includes") => {
                let needle = args.first().cloned().unwrap_or_default().to_js_string();
                let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let start = string_index_to_byte(value, start);
                return Value::Bool(value[start..].contains(&needle));
            }
            (Value::String(value), "indexOf") => {
                let needle = args.first().cloned().unwrap_or_default().to_js_string();
                let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let start_byte = string_index_to_byte(value, start);
                let index = value
                    .get(start_byte..)
                    .and_then(|tail| tail.find(&needle).map(|offset| start_byte + offset))
                    .map(|byte| value[..byte].chars().count() as f64)
                    .unwrap_or(-1.0);
                return Value::Number(index);
            }
            (Value::String(value), "charCodeAt") => {
                let index = args.first().map(Value::to_number).unwrap_or(0.0);
                if !index.is_finite() || index < 0.0 {
                    return Value::Number(f64::NAN);
                }
                return Value::Number(
                    value
                        .encode_utf16()
                        .nth(index as usize)
                        .map(f64::from)
                        .unwrap_or(f64::NAN),
                );
            }
            (Value::String(value), "charAt") => {
                let index = args.first().map(Value::to_number).unwrap_or(0.0);
                if !index.is_finite() || index < 0.0 {
                    return Value::String(String::new());
                }
                return Value::String(
                    value
                        .chars()
                        .nth(index as usize)
                        .map(|character| character.to_string())
                        .unwrap_or_default(),
                );
            }
            (Value::String(value), "substring") => {
                let len = value.chars().count();
                let mut start = args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                let mut end = args
                    .get(1)
                    .map(Value::to_number)
                    .unwrap_or(len as f64)
                    .max(0.0) as usize;
                start = start.min(len);
                end = end.min(len);
                if start > end {
                    std::mem::swap(&mut start, &mut end);
                }
                let start = string_index_to_byte(value, start);
                let end = string_index_to_byte(value, end);
                return Value::String(value[start..end].to_string());
            }
            (Value::String(value), "toUpperCase") => {
                return Value::String(value.to_uppercase());
            }
            (Value::String(value), "toLowerCase") => {
                return Value::String(value.to_lowercase());
            }
            (Value::String(value), "trim") => {
                return Value::String(value.trim().to_string());
            }
            (Value::String(value), "split") => {
                let Some(separator) = args.first() else {
                    return Value::array(vec![Value::String(value.clone())]);
                };
                if separator.is_undefined() {
                    return Value::array(vec![Value::String(value.clone())]);
                }
                let separator = separator.to_js_string();
                let limit = args
                    .get(1)
                    .map(|value| value.to_number().max(0.0) as usize)
                    .unwrap_or(usize::MAX);
                let parts: Vec<Value> = if separator.is_empty() {
                    value
                        .chars()
                        .take(limit)
                        .map(|ch| Value::String(ch.to_string()))
                        .collect()
                } else {
                    value
                        .split(&separator)
                        .take(limit)
                        .map(|part| Value::String(part.to_string()))
                        .collect()
                };
                return Value::array(parts);
            }
            (Value::String(value), "match") => {
                let pattern = args.first().cloned().unwrap_or(Value::Undefined);
                if let Some(result) = crate::regexp::string_match(value, &pattern) {
                    return result;
                }
            }
            (Value::String(value), "replace") => {
                let pattern = args.first().cloned().unwrap_or(Value::Undefined);
                let replacement = args.get(1).cloned().unwrap_or(Value::Undefined);
                if let Some(result) =
                    crate::regexp::string_replace(value, &pattern, &replacement.to_js_string())
                {
                    return result;
                }
                return Value::String(value.replacen(
                    &pattern.to_js_string(),
                    &replacement.to_js_string(),
                    1,
                ));
            }
            (Value::Array(values), "filter") => {
                let predicate = args.first().cloned().unwrap_or(Value::Undefined);
                let filtered = values
                    .borrow()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, value)| {
                        predicate
                            .call(
                                Value::Undefined,
                                vec![value.clone(), Value::Number(index as f64)],
                            )
                            .to_bool()
                            .then(|| value.clone())
                    })
                    .collect();
                return Value::array(filtered);
            }
            (Value::Array(values), "push") => {
                let mut values = values.borrow_mut();
                values.extend(args);
                return Value::Number(values.len() as f64);
            }
            (Value::Array(values), "set") => {
                let source: Vec<Value> = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .iter()
                    .collect();
                let offset = array_index(args.get(1), values.borrow().len(), 0);
                let mut target = values.borrow_mut();
                for (index, value) in source.into_iter().enumerate() {
                    if let Some(slot) = target.get_mut(offset + index) {
                        *slot = value;
                    }
                }
                return Value::Undefined;
            }
            (Value::Array(values), "forEach") => {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                for (index, value) in values.borrow().iter().cloned().enumerate() {
                    callback.call(
                        Value::Undefined,
                        vec![value, Value::Number(index as f64), self.clone()],
                    );
                }
                return Value::Undefined;
            }
            _ => {}
        }
        self.get_property(key).call(self.clone(), args)
    }

    pub fn iter(&self) -> std::vec::IntoIter<Value> {
        match self {
            Value::Array(values) => values.borrow().clone().into_iter(),
            // First use the standards-oriented Map/Set registry. Retain the
            // host runtime's snapshot hook as a fallback for its lightweight
            // built-in Map used by compiled React/native paths.
            Value::Object(object) => {
                if let Some(values) = crate::collections::iter_collection(self) {
                    return values.into_iter();
                }
                // Monaco's command registry stores commands in its own
                // LinkedList implementation. Generator lowering is still a
                // best-effort path, so expose that conventional `_first` /
                // `next` node chain through the runtime iterator bridge.
                let first = self.get_property("_first");
                if first.is_object() {
                    let mut values = Vec::new();
                    let mut node = first;
                    while node.is_object() {
                        let element = node.get_property("element");
                        if element.is_undefined() {
                            break;
                        }
                        values.push(element);
                        let next = node.get_property("next");
                        if next.strict_eq(&node) {
                            break;
                        }
                        node = next;
                    }
                    return values.into_iter();
                }
                // The AOT lowering turns common `Map#forEach(value => ...)`
                // loops into this iterator path. Map exposes a live values
                // snapshot so the lowered loop retains JavaScript semantics.
                let snapshot = object.borrow().get_direct("__w3cosMapValuesSnapshot");
                let values = if snapshot.is_function() {
                    snapshot.call(self.clone(), vec![])
                } else {
                    object.borrow().get_direct("__w3cosMapValues")
                };
                match values {
                    Value::Array(values) => values.borrow().clone().into_iter(),
                    _ => Vec::new().into_iter(),
                }
            }
            _ => Vec::new().into_iter(),
        }
    }
}

fn iterator_object(values: Vec<Value>) -> Value {
    let values = Rc::new(values);
    let index = Rc::new(RefCell::new(0usize));
    let next_values = values.clone();
    let next_index = index.clone();
    let next = Value::function(move |_, _| {
        let mut index = next_index.borrow_mut();
        if let Some(value) = next_values.get(*index).cloned() {
            *index += 1;
            crate::js_object! {
                "value" => value,
                "done" => Value::Bool(false),
            }
        } else {
            crate::js_object! {
                "value" => Value::Undefined,
                "done" => Value::Bool(true),
            }
        }
    });
    crate::js_object! { "next" => next }
}

/// Normalize a JS array index argument (`undefined` → `default`; negatives
/// wrap from the end; clamped to `len`).
fn array_index(value: Option<&Value>, len: usize, default: usize) -> usize {
    let Some(value) = value else {
        return default.min(len);
    };
    if value.is_undefined() {
        return default.min(len);
    }
    let n = value.to_number();
    if n.is_nan() {
        0
    } else if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        (n as usize).min(len)
    }
}

fn string_index_to_byte(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

/// The JS `Array.prototype` method set for [`Value::Array`]. Returns `None`
/// for names the dedicated match arms in [`Value::call_method`] implement
/// (`filter`/`push`/`forEach`) or don't cover at all.
fn array_call_method(
    values: &Rc<RefCell<Vec<Value>>>,
    key: &str,
    args: Vec<Value>,
    this: &Value,
) -> Option<Value> {
    let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Undefined);
    let callback_args = |value: &Value, index: usize| {
        vec![value.clone(), Value::Number(index as f64), this.clone()]
    };
    Some(match key {
        "filter" | "push" | "forEach" | "set" => return None, // handled by dedicated arms
        "pop" => values.borrow_mut().pop().unwrap_or(Value::Undefined),
        "shift" => {
            if values.borrow().is_empty() {
                Value::Undefined
            } else {
                values.borrow_mut().remove(0)
            }
        }
        "unshift" => {
            let mut values = values.borrow_mut();
            for (offset, item) in args.iter().enumerate() {
                values.insert(offset, item.clone());
            }
            Value::Number(values.len() as f64)
        }
        "slice" => {
            let values = values.borrow();
            let start = array_index(args.first(), values.len(), 0);
            let end = array_index(args.get(1), values.len(), values.len());
            Value::array(values[start.min(end)..end].to_vec())
        }
        "splice" => {
            let mut values = values.borrow_mut();
            let start = array_index(args.first(), values.len(), 0);
            let delete_count = match args.get(1) {
                None => values.len() - start,
                Some(v) if v.is_undefined() => values.len() - start,
                Some(v) => (v.to_number().max(0.0) as usize).min(values.len() - start),
            };
            let mut tail = values.split_off(start);
            let removed: Vec<Value> = tail.drain(..delete_count.min(tail.len())).collect();
            for (offset, item) in args.iter().skip(2).enumerate() {
                tail.insert(offset, item.clone());
            }
            values.extend(tail);
            Value::array(removed)
        }
        "map" => {
            let f = arg(0);
            let mapped = values
                .borrow()
                .iter()
                .enumerate()
                .map(|(index, value)| f.call(Value::Undefined, callback_args(value, index)))
                .collect();
            Value::array(mapped)
        }
        "find" => {
            let f = arg(0);
            values
                .borrow()
                .iter()
                .enumerate()
                .find(|(index, value)| {
                    f.call(Value::Undefined, callback_args(value, *index))
                        .to_bool()
                })
                .map(|(_, value)| value.clone())
                .unwrap_or(Value::Undefined)
        }
        "findIndex" => {
            let f = arg(0);
            let index = values
                .borrow()
                .iter()
                .enumerate()
                .find(|(index, value)| {
                    f.call(Value::Undefined, callback_args(value, *index))
                        .to_bool()
                })
                .map(|(index, _)| index as f64)
                .unwrap_or(-1.0);
            Value::Number(index)
        }
        "some" => {
            let f = arg(0);
            let hit = values.borrow().iter().enumerate().any(|(index, value)| {
                f.call(Value::Undefined, callback_args(value, index))
                    .to_bool()
            });
            Value::Bool(hit)
        }
        "every" => {
            let f = arg(0);
            let all = values.borrow().iter().enumerate().all(|(index, value)| {
                f.call(Value::Undefined, callback_args(value, index))
                    .to_bool()
            });
            Value::Bool(all)
        }
        "includes" => {
            let needle = arg(0);
            let hit = values.borrow().iter().any(|value| value.strict_eq(&needle));
            Value::Bool(hit)
        }
        "indexOf" => {
            let needle = arg(0);
            let index = values
                .borrow()
                .iter()
                .position(|value| value.strict_eq(&needle))
                .map(|index| index as f64)
                .unwrap_or(-1.0);
            Value::Number(index)
        }
        "lastIndexOf" => {
            let needle = arg(0);
            let index = values
                .borrow()
                .iter()
                .rposition(|value| value.strict_eq(&needle))
                .map(|index| index as f64)
                .unwrap_or(-1.0);
            Value::Number(index)
        }
        "join" => {
            let separator = match args.first() {
                None => ",".to_string(),
                Some(v) if v.is_undefined() => ",".to_string(),
                Some(v) => v.to_js_string(),
            };
            Value::from(
                values
                    .borrow()
                    .iter()
                    .map(|value| {
                        if value.is_nullish() {
                            String::new()
                        } else {
                            value.to_js_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(&separator),
            )
        }
        "concat" => {
            let mut out = values.borrow().clone();
            for item in &args {
                match item {
                    Value::Array(inner) => out.extend(inner.borrow().iter().cloned()),
                    other => out.push(other.clone()),
                }
            }
            Value::array(out)
        }
        "reduce" => {
            let f = arg(0);
            let values = values.borrow();
            let (mut acc, start) = match args.get(1) {
                Some(init) => (init.clone(), 0),
                None => match values.first() {
                    Some(first) => (first.clone(), 1),
                    None => return Some(Value::Undefined),
                },
            };
            for (index, value) in values.iter().enumerate().skip(start) {
                acc = f.call(
                    Value::Undefined,
                    vec![
                        acc,
                        value.clone(),
                        Value::Number(index as f64),
                        this.clone(),
                    ],
                );
            }
            acc
        }
        "reduceRight" => {
            let f = arg(0);
            let values = values.borrow();
            let (mut acc, start) = match args.get(1) {
                Some(init) => (init.clone(), values.len()),
                None => match values.last() {
                    Some(last) => (last.clone(), values.len().saturating_sub(1)),
                    None => return Some(Value::Undefined),
                },
            };
            for index in (0..start).rev() {
                acc = f.call(
                    Value::Undefined,
                    vec![
                        acc,
                        values[index].clone(),
                        Value::Number(index as f64),
                        this.clone(),
                    ],
                );
            }
            acc
        }
        "sort" => {
            let comparator = args.first().cloned();
            let mut sorted = values.borrow().clone();
            sorted.sort_by(|left, right| match &comparator {
                Some(f) if !f.is_undefined() => {
                    let order = f
                        .call(Value::Undefined, vec![left.clone(), right.clone()])
                        .to_number();
                    order.total_cmp(&0.0)
                }
                _ => left.to_js_string().cmp(&right.to_js_string()),
            });
            *values.borrow_mut() = sorted;
            this.clone()
        }
        "reverse" => {
            values.borrow_mut().reverse();
            this.clone()
        }
        "flat" => {
            let depth = args
                .first()
                .map(|v| v.to_number().max(0.0) as usize)
                .unwrap_or(1);
            fn flatten(into: &mut Vec<Value>, items: &[Value], depth: usize) {
                for item in items {
                    match item {
                        Value::Array(inner) if depth > 0 => {
                            flatten(into, &inner.borrow(), depth - 1)
                        }
                        other => into.push(other.clone()),
                    }
                }
            }
            let mut out = Vec::new();
            flatten(&mut out, &values.borrow(), depth);
            Value::array(out)
        }
        "flatMap" => {
            let f = arg(0);
            let mut out = Vec::new();
            for (index, value) in values.borrow().iter().enumerate() {
                match f.call(Value::Undefined, callback_args(value, index)) {
                    Value::Array(inner) => out.extend(inner.borrow().iter().cloned()),
                    other => out.push(other),
                }
            }
            Value::array(out)
        }
        "at" => {
            let values = values.borrow();
            let index = array_index(args.first(), values.len(), values.len());
            values.get(index).cloned().unwrap_or(Value::Undefined)
        }
        _ => return None,
    })
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(value.to_string())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Number(value)
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Value::Number(value as f64)
    }
}

macro_rules! impl_number_value_from {
    ($($ty:ty),* $(,)?) => {
        $(
            impl From<$ty> for Value {
                fn from(value: $ty) -> Self {
                    Value::Number(value as f64)
                }
            }
        )*
    };
}

impl_number_value_from!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Value::array(value)
    }
}

#[macro_export]
macro_rules! js_object {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut properties = ::std::collections::HashMap::new();
        $(properties.insert(($key).to_string(), $crate::Value::from($value));)*
        $crate::Value::object(properties)
    }};
}

// ── Arithmetic / comparison operators ──────────────────────────────────

impl Value {
    /// ECMAScript `+` (addition or string concatenation).
    pub fn js_add(&self, other: &Value) -> Value {
        if self.is_string() || other.is_string() {
            Value::String(format!("{}{}", self.to_js_string(), other.to_js_string()))
        } else {
            Value::Number(self.to_number() + other.to_number())
        }
    }

    pub fn js_sub(&self, other: &Value) -> Value {
        Value::Number(self.to_number() - other.to_number())
    }

    pub fn js_mul(&self, other: &Value) -> Value {
        Value::Number(self.to_number() * other.to_number())
    }

    pub fn js_div(&self, other: &Value) -> Value {
        Value::Number(self.to_number() / other.to_number())
    }

    pub fn js_rem(&self, other: &Value) -> Value {
        Value::Number(self.to_number() % other.to_number())
    }

    pub fn js_neg(&self) -> Value {
        Value::Number(-self.to_number())
    }

    pub fn js_pow(&self, other: &Value) -> Value {
        Value::Number(self.to_number().powf(other.to_number()))
    }

    // ── Bitwise operators ──

    pub fn js_bitor(&self, other: &Value) -> Value {
        Value::Number((self.to_i32() | other.to_i32()) as f64)
    }

    pub fn js_bitand(&self, other: &Value) -> Value {
        Value::Number((self.to_i32() & other.to_i32()) as f64)
    }

    pub fn js_bitxor(&self, other: &Value) -> Value {
        Value::Number((self.to_i32() ^ other.to_i32()) as f64)
    }

    pub fn js_bitnot(&self) -> Value {
        Value::Number((!self.to_i32()) as f64)
    }

    pub fn js_shl(&self, other: &Value) -> Value {
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number((self.to_i32() << shift) as f64)
    }

    pub fn js_shr(&self, other: &Value) -> Value {
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number((self.to_i32() >> shift) as f64)
    }

    pub fn js_ushr(&self, other: &Value) -> Value {
        let shift = (other.to_i32() as u32) & 0x1f;
        Value::Number(((self.to_i32() as u32) >> shift) as f64)
    }

    /// ECMAScript `===` (strict equality).
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) => true,
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => {
                if a.is_nan() || b.is_nan() {
                    false
                } else {
                    a == b
                }
            }
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Function(a), Value::Function(b)) => a.ptr_eq(b),
            _ => false,
        }
    }

    /// ECMAScript SameValueZero (Map/Set key equality): strict equality for
    /// primitives except NaN equals NaN and -0 equals +0; Array/Object keys
    /// compare by reference identity (`Rc` pointer), Function keys by shared
    /// closure identity (clones of one function value are the same key).
    pub fn same_value_zero(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            // f64 `==` already identifies -0.0 with +0.0 (SameValueZero
            // agrees); NaN needs the explicit special-case.
            (Value::Number(a), Value::Number(b)) => a == b || (a.is_nan() && b.is_nan()),
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Function(a), Value::Function(b)) => a.ptr_eq(b),
            _ => false,
        }
    }

    /// ECMAScript `==` (abstract equality — simplified).
    pub fn abstract_eq(&self, other: &Value) -> bool {
        if std::mem::discriminant(self) == std::mem::discriminant(other) {
            return self.strict_eq(other);
        }
        match (self, other) {
            (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,
            (Value::Number(_), Value::String(_)) => {
                self.strict_eq(&Value::Number(other.to_number()))
            }
            (Value::String(_), Value::Number(_)) => {
                Value::Number(self.to_number()).strict_eq(other)
            }
            (Value::Bool(_), _) => Value::Number(self.to_number()).abstract_eq(other),
            (_, Value::Bool(_)) => self.abstract_eq(&Value::Number(other.to_number())),
            _ => false,
        }
    }

    pub fn js_lt(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::String(left), Value::String(right)) => left < right,
            _ => self.to_number() < other.to_number(),
        }
    }

    pub fn js_gt(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::String(left), Value::String(right)) => left > right,
            _ => self.to_number() > other.to_number(),
        }
    }

    pub fn js_le(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::String(left), Value::String(right)) => left <= right,
            _ => self.to_number() <= other.to_number(),
        }
    }

    pub fn js_ge(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::String(left), Value::String(right)) => left >= right,
            _ => self.to_number() >= other.to_number(),
        }
    }

    pub fn js_not(&self) -> Value {
        Value::Bool(!self.to_bool())
    }
}

// ── Display ────────────────────────────────────────────────────────────

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_js_string())
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{n}"),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Array(arr) => write!(f, "{:?}", arr.borrow()),
            Value::Object(_) => write!(f, "{{...}}"),
            Value::Function(_) => write!(f, "[Function]"),
        }
    }
}

/// Standalone `type_of` function matching the generated code's `type_of(expr)` calls.
pub fn type_of(val: &Value) -> Value {
    Value::String(val.type_of().to_string())
}

/// An Error-shaped object (`{ message }`) for runtime failures. Compiled JS
/// raises exceptions via `std::panic::panic_any`; builtins that need to
/// signal a JS exception (invalid JSON, circular structures, bad URLs)
/// build one of these and [`throw_value`] it so compiled `try/catch` sees
/// a JS-style error value.
pub(crate) fn js_error(message: &str) -> Value {
    let mut properties = HashMap::new();
    properties.insert("message".to_string(), Value::String(message.to_string()));
    Value::object(properties)
}

/// Panic payload for JS exceptions.
///
/// `std::panic::panic_any` requires a `Send` payload and `Value` is not
/// `Send` (it holds `Rc`s), so JS `throw` cannot panic with a bare
/// `Value`. This newtype wraps it; the `Send` impl is sound here because
/// the runtime is single-threaded — the payload only ever travels from a
/// `throw_value` call site to a `catch_unwind` on the same thread.
pub struct PanicValue(pub Value);

// SAFETY: w3cos values are single-threaded by design (Rc/RefCell
// everywhere); the wrapper never crosses an actual thread boundary.
unsafe impl Send for PanicValue {}

/// Raise a JS exception: `throw value` in compiled code and in builtins.
/// Unwinds until a `catch_unwind` (compiled `try/catch`, or the promise
/// reaction runner, which turns it into a rejection).
pub fn throw_value(value: Value) -> ! {
    // Debug channel: W3COS_JS_CONSOLE=1 prints thrown values (incl. Error
    // objects with message/stack props) before unwinding — without it an
    // uncaught JS throw only shows Rust's opaque "Box<dyn Any>".
    if std::env::var_os("W3COS_JS_CONSOLE").is_some() {
        let message = value.get_property("message");
        let detail = if message.is_undefined() {
            value.to_js_string()
        } else {
            message.to_js_string()
        };
        eprintln!("[js.throw] {detail}");
    }
    std::panic::panic_any(PanicValue(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_coercion() {
        assert_eq!(Value::Undefined.to_bool(), false);
        assert_eq!(Value::Null.to_bool(), false);
        assert_eq!(Value::Bool(false).to_bool(), false);
        assert_eq!(Value::Number(0.0).to_bool(), false);
        assert_eq!(Value::String("".into()).to_bool(), false);
        assert_eq!(Value::Number(1.0).to_bool(), true);
        assert_eq!(Value::String("x".into()).to_bool(), true);
    }

    #[test]
    fn type_of_values() {
        assert_eq!(Value::Undefined.type_of(), "undefined");
        assert_eq!(Value::Null.type_of(), "object");
        assert_eq!(Value::Number(42.0).type_of(), "number");
        assert_eq!(Value::String("hi".into()).type_of(), "string");
        assert_eq!(Value::Bool(true).type_of(), "boolean");
    }

    #[test]
    fn arithmetic() {
        let a = Value::Number(10.0);
        let b = Value::Number(3.0);
        assert_eq!(a.js_add(&b).to_number(), 13.0);
        assert_eq!(a.js_sub(&b).to_number(), 7.0);
        assert_eq!(a.js_mul(&b).to_number(), 30.0);
    }

    #[test]
    fn string_concat() {
        let a = Value::String("hello".into());
        let b = Value::Number(42.0);
        assert_eq!(a.js_add(&b).to_js_string(), "hello42");
    }

    #[test]
    fn string_methods_cover_token_parsing() {
        let token = Value::String("source.ts".into());
        assert_eq!(
            token
                .call_method("indexOf", vec![Value::from(".")])
                .to_number(),
            6.0
        );
        assert_eq!(
            token
                .call_method("substring", vec![Value::Number(7.0)])
                .to_js_string(),
            "ts"
        );
        assert_eq!(
            Value::from("a😀c")
                .call_method("substr", vec![Value::Number(1.0), Value::Number(2.0)])
                .to_js_string(),
            "😀"
        );
        assert_eq!(
            token
                .call_method("substr", vec![Value::Number(-2.0)])
                .to_js_string(),
            "ts"
        );
        assert_eq!(
            Value::from("abc")
                .call_method("toUpperCase", vec![])
                .to_js_string(),
            "ABC"
        );
        assert_eq!(
            Value::from("a b")
                .call_method("split", vec![Value::from(" ")])
                .get_property("1")
                .to_js_string(),
            "b"
        );
        assert_eq!(
            Value::from("a\n").call_method("charCodeAt", vec![Value::Number(1.0)]),
            Value::Number(10.0)
        );
        assert!(
            Value::from("a")
                .call_method("charCodeAt", vec![Value::Number(2.0)])
                .to_number()
                .is_nan()
        );
    }

    #[test]
    fn strict_equality() {
        assert!(Value::Number(1.0).strict_eq(&Value::Number(1.0)));
        assert!(!Value::Number(f64::NAN).strict_eq(&Value::Number(f64::NAN)));
        assert!(Value::String("a".into()).strict_eq(&Value::String("a".into())));
        assert!(!Value::Number(1.0).strict_eq(&Value::String("1".into())));

        let array = Value::array(vec![]);
        let other_array = Value::array(vec![]);
        assert!(array.strict_eq(&array.clone()));
        assert!(!array.strict_eq(&other_array));

        let object = Value::object(HashMap::new());
        let other_object = Value::object(HashMap::new());
        assert!(object.strict_eq(&object.clone()));
        assert!(!object.strict_eq(&other_object));

        let function = Value::function(|_, _| Value::Undefined);
        let other_function = Value::function(|_, _| Value::Undefined);
        assert!(function.strict_eq(&function.clone()));
        assert!(!function.strict_eq(&other_function));
    }

    #[test]
    fn identity_hash_tracks_heap_identity_and_object_is_numbers() {
        let function = Value::function(|_, _| Value::Undefined);
        assert_eq!(function.identity_hash(), function.clone().identity_hash());
        assert_ne!(
            function.identity_hash(),
            Value::function(|_, _| Value::Undefined).identity_hash()
        );

        let object = Value::object(HashMap::new());
        assert_eq!(object.identity_hash(), object.clone().identity_hash());
        assert_ne!(
            object.identity_hash(),
            Value::object(HashMap::new()).identity_hash()
        );

        assert_eq!(
            Value::Number(f64::NAN).identity_hash(),
            Value::Number(-f64::NAN).identity_hash()
        );
        assert_ne!(
            Value::Number(0.0).identity_hash(),
            Value::Number(-0.0).identity_hash()
        );
    }

    #[test]
    fn abstract_equality() {
        assert!(Value::Null.abstract_eq(&Value::Undefined));
        assert!(Value::Number(1.0).abstract_eq(&Value::String("1".into())));
        assert!(Value::Bool(true).abstract_eq(&Value::Number(1.0)));
    }

    #[test]
    fn relational_comparison_is_lexicographic_for_two_strings() {
        assert!(Value::from("function").js_lt(&Value::from("u")));
        assert!(Value::from("10").js_lt(&Value::from("2")));
        assert!(Value::from("u").js_ge(&Value::from("u")));
        assert!(!Value::from("z").js_le(&Value::from("a")));
        assert!(Value::from("10").js_gt(&Value::Number(2.0)));
    }

    #[test]
    fn to_js_string() {
        assert_eq!(Value::Undefined.to_js_string(), "undefined");
        assert_eq!(Value::Null.to_js_string(), "null");
        assert_eq!(Value::Number(42.0).to_js_string(), "42");
        assert_eq!(Value::Number(3.14).to_js_string(), "3.14");
        assert_eq!(Value::Bool(true).to_js_string(), "true");

        let named_function = Value::function(|_, _| Value::Undefined);
        named_function.set_property(
            "toString",
            Value::function(|_, _| Value::String("modelService".into())),
        );
        assert_eq!(named_function.to_js_string(), "modelService");

        let plain_function = Value::function(|_, _| Value::Undefined);
        assert_eq!(
            plain_function.to_js_string(),
            "function() { [native code] }"
        );
    }

    #[test]
    fn bitwise_operations() {
        let a = Value::Number(5.0);
        let b = Value::Number(3.0);
        assert_eq!(a.js_bitor(&b).to_number(), 7.0);
        assert_eq!(a.js_bitand(&b).to_number(), 1.0);
        assert_eq!(a.js_bitxor(&b).to_number(), 6.0);
        assert_eq!(a.js_shl(&Value::Number(1.0)).to_number(), 10.0);
        assert_eq!(a.js_shr(&Value::Number(1.0)).to_number(), 2.0);
    }

    #[test]
    fn power_operator() {
        assert_eq!(
            Value::Number(2.0).js_pow(&Value::Number(10.0)).to_number(),
            1024.0
        );
        assert_eq!(
            Value::Number(9.0).js_pow(&Value::Number(0.5)).to_number(),
            3.0
        );
    }

    #[test]
    fn function_call_apply_and_bind_preserve_receiver_and_arguments() {
        let function = Value::function(|this, args| {
            Value::Number(
                this.get_property("base").to_number()
                    + args.iter().map(Value::to_number).sum::<f64>(),
            )
        });
        let receiver = Value::object(HashMap::from([("base".to_string(), Value::Number(10.0))]));

        assert_eq!(
            function
                .call_method(
                    "call",
                    vec![receiver.clone(), Value::Number(2.0), Value::Number(3.0)],
                )
                .to_number(),
            15.0
        );
        assert_eq!(
            function
                .call_method(
                    "apply",
                    vec![
                        receiver.clone(),
                        Value::array(vec![Value::Number(4.0), Value::Number(5.0)]),
                    ],
                )
                .to_number(),
            19.0
        );
        let bound = function.call_method("bind", vec![receiver, Value::Number(6.0)]);
        assert_eq!(
            bound
                .call(Value::Undefined, vec![Value::Number(7.0)])
                .to_number(),
            23.0
        );
    }

    #[test]
    fn to_i32_conversion() {
        assert_eq!(Value::Number(42.7).to_i32(), 42);
        assert_eq!(Value::Number(-3.9).to_i32(), -3);
        assert_eq!(Value::Number(f64::NAN).to_i32(), 0);
        assert_eq!(Value::Number(f64::INFINITY).to_i32(), 0);
    }

    #[test]
    fn in_operator() {
        let mut props = HashMap::new();
        props.insert("name".to_string(), Value::String("test".into()));
        let obj = Value::object(props);
        assert!(Value::String("name".into()).js_in(&obj).to_bool());
        assert!(!Value::String("age".into()).js_in(&obj).to_bool());

        let arr = Value::array(vec![Value::Number(10.0), Value::Number(20.0)]);
        assert!(Value::Number(0.0).js_in(&arr).to_bool());
        assert!(Value::Number(1.0).js_in(&arr).to_bool());
        assert!(!Value::Number(2.0).js_in(&arr).to_bool());
    }

    #[test]
    fn js_object_macro_builds_dynamic_properties() {
        let object = crate::js_object! {
            "rowCount" => 1_000,
            "label" => "rows",
            "enabled" => true,
        };
        assert_eq!(object.get_property("rowCount").to_number(), 1_000.0);
        assert_eq!(object.get_property("label").to_js_string(), "rows");
        assert!(object.get_property("enabled").to_bool());
    }

    #[test]
    fn dynamic_function_call_preserves_receiver() {
        let receiver = crate::js_object! { "value" => 42 };
        receiver.set_property(
            "read",
            Value::function(|this, _| this.get_property("value")),
        );
        assert_eq!(receiver.call_method("read", vec![]).to_number(), 42.0);
    }

    #[test]
    fn symbol_iterator_walks_linked_list_style_objects() {
        let sentinel = crate::js_object! { "element" => Value::Undefined };
        let second = crate::js_object! {
            "element" => "second",
            "next" => sentinel,
        };
        let first = crate::js_object! {
            "element" => "first",
            "next" => second,
        };
        let list = crate::js_object! { "_first" => first };

        let iterator = list.call_method("__w3cos_symbol_iterator", vec![]);
        let first_result = iterator.call_method("next", vec![]);
        let second_result = iterator.call_method("next", vec![]);
        let done_result = iterator.call_method("next", vec![]);

        assert_eq!(first_result.get_property("value").to_js_string(), "first");
        assert!(!first_result.get_property("done").to_bool());
        assert_eq!(second_result.get_property("value").to_js_string(), "second");
        assert!(done_result.get_property("done").to_bool());
    }

    #[test]
    fn property_access() {
        let mut props = HashMap::new();
        props.insert("x".to_string(), Value::Number(42.0));
        let obj = Value::object(props);
        assert_eq!(obj.get_property("x").to_number(), 42.0);
        assert!(obj.get_property("y").is_undefined());

        let arr = Value::array(vec![Value::String("a".into()), Value::String("b".into())]);
        assert_eq!(arr.get_property("0").to_js_string(), "a");
        assert_eq!(arr.get_property("length").to_number(), 2.0);

        let s = Value::String("hello".into());
        assert_eq!(s.get_property("length").to_number(), 5.0);
        assert_eq!(s.get_property("0").to_js_string(), "h");

        let chinese = Value::String("请提前到达，卸货前联系我".into());
        assert_eq!(chinese.get_property("length").to_number(), 12.0);
        assert_eq!(
            chinese
                .call_method("slice", vec![Value::Number(0.0), Value::Number(6.0)])
                .to_js_string(),
            "请提前到达，",
        );
        assert_eq!(
            chinese
                .call_method("slice", vec![Value::Number(-6.0)])
                .to_js_string(),
            "卸货前联系我",
        );
    }

    #[test]
    fn encoded_getters_remain_visible_after_plain_object_fast_path() {
        let object = Value::object(HashMap::new());
        object.set_property(
            "__w3cos_getter_label",
            Value::function(|_, _| Value::from("computed")),
        );

        assert_eq!(object.get_property("label").to_js_string(), "computed");
        assert!(object.get_property("missing").is_undefined());
    }

    #[test]
    fn object_rest_excludes_destructured_own_properties() {
        let object = Value::object(HashMap::from([
            ("ariaAttributes".into(), Value::from("aria")),
            ("style".into(), Value::from("style")),
            ("index".into(), Value::Number(7.0)),
        ]));

        let rest = object.object_rest(&["ariaAttributes", "style"]);

        assert!(rest.get_property("ariaAttributes").is_undefined());
        assert!(rest.get_property("style").is_undefined());
        assert_eq!(rest.get_property("index").to_number(), 7.0);
    }

    #[test]
    fn property_set() {
        let obj = Value::object(HashMap::new());
        obj.set_property("key", Value::Number(99.0));
        assert_eq!(obj.get_property("key").to_number(), 99.0);
    }

    #[test]
    fn delete_removes_object_property() {
        let value = Value::object(HashMap::from([("model".into(), Value::Number(1.0))]));
        assert!(value.delete_property("model").to_bool());
        assert!(value.get_property("model").is_undefined());
    }
}
