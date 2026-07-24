//! ArrayBuffer, DataView, and typed-array backing-store semantics.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::Value;

const BUFFER_STATE_KEY: &str = "__w3cos_array_buffer_id";
const SHARED_BUFFER_KEY: &str = "__w3cos_shared_array_buffer";

pub const TYPED_ARRAY_NAMES: &[&str] = &[
    "Uint8Array",
    "Uint8ClampedArray",
    "Int8Array",
    "Uint16Array",
    "Int16Array",
    "Uint32Array",
    "Int32Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Uint8,
    Uint8Clamped,
    Int8,
    Uint16,
    Int16,
    Uint32,
    Int32,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl Kind {
    fn from_name(name: &str) -> Self {
        match name {
            "Uint8ClampedArray" => Self::Uint8Clamped,
            "Int8Array" => Self::Int8,
            "Uint16Array" => Self::Uint16,
            "Int16Array" => Self::Int16,
            "Uint32Array" => Self::Uint32,
            "Int32Array" => Self::Int32,
            "Float32Array" => Self::Float32,
            "Float64Array" => Self::Float64,
            "BigInt64Array" => Self::BigInt64,
            "BigUint64Array" => Self::BigUint64,
            _ => Self::Uint8,
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::Uint8 | Self::Uint8Clamped | Self::Int8 => 1,
            Self::Uint16 | Self::Int16 => 2,
            Self::Uint32 | Self::Int32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    fn supports_atomics(self) -> bool {
        !matches!(self, Self::Uint8Clamped | Self::Float32 | Self::Float64)
    }

    fn supports_wait(self) -> bool {
        matches!(self, Self::Int32 | Self::BigInt64)
    }

    fn decode(self, bytes: &[u8]) -> Value {
        match self {
            Self::BigInt64 => {
                return crate::bigint::parse(
                    &i64::from_le_bytes(bytes.try_into().expect("eight bytes")).to_string(),
                );
            }
            Self::BigUint64 => {
                return crate::bigint::parse(
                    &u64::from_le_bytes(bytes.try_into().expect("eight bytes")).to_string(),
                );
            }
            _ => {}
        }
        let number = match self {
            Self::Uint8 | Self::Uint8Clamped => bytes[0] as f64,
            Self::Int8 => (bytes[0] as i8) as f64,
            Self::Uint16 => u16::from_le_bytes([bytes[0], bytes[1]]) as f64,
            Self::Int16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f64,
            Self::Uint32 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
            Self::Int32 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
            Self::Float32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
            Self::Float64 => f64::from_le_bytes(bytes.try_into().expect("eight bytes")),
            Self::BigInt64 | Self::BigUint64 => unreachable!("handled above"),
        };
        Value::Number(number)
    }

    fn encode(self, value: &Value) -> Vec<u8> {
        match self {
            Self::BigInt64 => {
                return crate::bigint::to_i64(value)
                    .unwrap_or_else(|| {
                        crate::throw_value(Value::object(HashMap::from([(
                            "name".into(),
                            Value::string("TypeError"),
                        )])))
                    })
                    .to_le_bytes()
                    .to_vec();
            }
            Self::BigUint64 => {
                return crate::bigint::to_u64(value)
                    .unwrap_or_else(|| {
                        crate::throw_value(Value::object(HashMap::from([(
                            "name".into(),
                            Value::string("TypeError"),
                        )])))
                    })
                    .to_le_bytes()
                    .to_vec();
            }
            _ => {}
        }
        let number = value.to_number();
        match self {
            Self::Uint8 => vec![(number as i64) as u8],
            Self::Uint8Clamped => vec![number.round().clamp(0.0, 255.0) as u8],
            Self::Int8 => vec![(number as i64) as i8 as u8],
            Self::Uint16 => (number as u16).to_le_bytes().to_vec(),
            Self::Int16 => (number as i16).to_le_bytes().to_vec(),
            Self::Uint32 => (number as u32).to_le_bytes().to_vec(),
            Self::Int32 => (number as i32).to_le_bytes().to_vec(),
            Self::Float32 => (number as f32).to_le_bytes().to_vec(),
            Self::Float64 => number.to_le_bytes().to_vec(),
            Self::BigInt64 | Self::BigUint64 => unreachable!("handled above"),
        }
    }
}

struct View {
    storage: Weak<RefCell<Vec<Value>>>,
    buffer: Value,
    bytes: Rc<RefCell<Vec<u8>>>,
    offset: usize,
    length: usize,
    kind: Kind,
}

thread_local! {
    static NEXT_BUFFER_ID: RefCell<u64> = const { RefCell::new(1) };
    static BUFFERS: RefCell<HashMap<u64, Rc<RefCell<Vec<u8>>>>> = RefCell::new(HashMap::new());
    static VIEWS: RefCell<Vec<View>> = const { RefCell::new(Vec::new()) };
    static CLASSES: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
    static ARRAY_BUFFER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SHARED_ARRAY_BUFFER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DATA_VIEW_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ATOMICS_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ATOMICS_WAIT_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
}

fn buffer_state(value: &Value) -> Option<Rc<RefCell<Vec<u8>>>> {
    let Value::Object(object) = value else {
        return None;
    };
    let Value::Number(id) = object.borrow().get_direct(BUFFER_STATE_KEY) else {
        return None;
    };
    BUFFERS.with(|buffers| buffers.borrow().get(&(id as u64)).cloned())
}

fn new_buffer_with_shared(bytes: Vec<u8>, shared: bool) -> Value {
    let state = Rc::new(RefCell::new(bytes));
    let id = NEXT_BUFFER_ID.with(|next| {
        let id = *next.borrow();
        *next.borrow_mut() = id + 1;
        id
    });
    BUFFERS.with(|buffers| buffers.borrow_mut().insert(id, state.clone()));
    let value = Value::object(HashMap::from([
        (BUFFER_STATE_KEY.to_string(), Value::Number(id as f64)),
        (SHARED_BUFFER_KEY.to_string(), Value::Bool(shared)),
    ]));
    let state_for_length = state.clone();
    value.set_property(
        "__w3cos_getter_byteLength",
        Value::function(move |_, _| Value::Number(state_for_length.borrow().len() as f64)),
    );
    let value_for_slice = value.clone();
    value.set_property(
        "slice",
        Value::function(move |_, args| {
            let bytes = buffer_state(&value_for_slice).expect("registered buffer");
            let bytes = bytes.borrow();
            let start = normalize_index(args.first(), bytes.len(), 0);
            let end = normalize_index(args.get(1), bytes.len(), bytes.len()).max(start);
            let contents = bytes[start..end].to_vec();
            if shared {
                let target = crate::class::construct(
                    &shared_array_buffer_class(),
                    vec![Value::Number(contents.len() as f64)],
                );
                if let Some(bytes) = buffer_state(&target) {
                    *bytes.borrow_mut() = contents;
                }
                target
            } else {
                array_buffer_value(contents)
            }
        }),
    );
    if !shared {
        let state_for_resize = state;
        value.set_property(
            "resize",
            Value::function(move |_, args| {
                let length = args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                state_for_resize.borrow_mut().resize(length, 0);
                sync_views(&state_for_resize);
                Value::Undefined
            }),
        );
    }
    value
}

fn new_buffer(bytes: Vec<u8>) -> Value {
    new_buffer_with_shared(bytes, false)
}

pub fn array_buffer_value(bytes: Vec<u8>) -> Value {
    new_buffer(bytes)
}

fn is_shared_buffer(value: &Value) -> bool {
    value.get_property(SHARED_BUFFER_KEY).to_bool()
}

pub fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    if let Some(bytes) = buffer_state(value) {
        return Some(bytes.borrow().clone());
    }
    let (bytes, offset, length, kind, _) = view_for(value)?;
    let end = offset + length * kind.bytes();
    Some(bytes.borrow()[offset..end].to_vec())
}

fn normalize_index(value: Option<&Value>, length: usize, fallback: usize) -> usize {
    let number = value.map(Value::to_number).unwrap_or(fallback as f64);
    if number.is_sign_negative() {
        (length as i64 + number as i64).max(0) as usize
    } else {
        (number.max(0.0) as usize).min(length)
    }
}

pub fn array_buffer_class() -> Value {
    ARRAY_BUFFER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let length = args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
            new_buffer(vec![0; length])
        });
        class.set_property(
            "isView",
            Value::function(|_, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                Value::Bool(
                    is_typed_array(&value) || value.get_property("__w3cos_data_view").to_bool(),
                )
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn shared_array_buffer_class() -> Value {
    SHARED_ARRAY_BUFFER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let prototype = Value::object(HashMap::new());
        let prototype_for_constructor = prototype.clone();
        let class = Value::function(move |_, args| {
            let length = args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
            let buffer = new_buffer_with_shared(vec![0; length], true);
            crate::class::set_prototype_of(&buffer, &prototype_for_constructor);
            buffer
        });
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn view_for(value: &Value) -> Option<(Rc<RefCell<Vec<u8>>>, usize, usize, Kind, Value)> {
    let Value::Array(candidate) = value else {
        return None;
    };
    VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        views.retain(|view| view.storage.strong_count() > 0);
        views.iter().find_map(|view| {
            let storage = view.storage.upgrade()?;
            Rc::ptr_eq(&storage, candidate).then(|| {
                (
                    view.bytes.clone(),
                    view.offset,
                    view.length,
                    view.kind,
                    view.buffer.clone(),
                )
            })
        })
    })
}

fn sync_views(changed: &Rc<RefCell<Vec<u8>>>) {
    VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        views.retain(|view| view.storage.strong_count() > 0);
        let bytes = changed.borrow();
        for view in views.iter() {
            if !Rc::ptr_eq(&view.bytes, changed) {
                continue;
            }
            let Some(storage) = view.storage.upgrade() else {
                continue;
            };
            let mut values = storage.borrow_mut();
            values.clear();
            for index in 0..view.length {
                let start = view.offset + index * view.kind.bytes();
                let end = start + view.kind.bytes();
                if end <= bytes.len() {
                    values.push(view.kind.decode(&bytes[start..end]));
                }
            }
        }
    });
}

fn new_view(buffer: Value, offset: usize, length: usize, kind: Kind) -> Value {
    let bytes = buffer_state(&buffer).expect("typed-array buffer");
    let values = {
        let bytes = bytes.borrow();
        (0..length)
            .map(|index| {
                let start = offset + index * kind.bytes();
                kind.decode(&bytes[start..start + kind.bytes()])
            })
            .collect()
    };
    let value = Value::array(values);
    let Value::Array(storage) = &value else {
        unreachable!()
    };
    VIEWS.with(|views| {
        views.borrow_mut().push(View {
            storage: Rc::downgrade(storage),
            buffer,
            bytes,
            offset,
            length,
            kind,
        })
    });
    value
}

fn construct_typed_array(kind: Kind, args: &[Value]) -> Value {
    let Some(first) = args.first() else {
        return new_view(new_buffer(Vec::new()), 0, 0, kind);
    };
    if let Some(bytes) = buffer_state(first) {
        let offset = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
        let available = bytes.borrow().len().saturating_sub(offset) / kind.bytes();
        let length = args
            .get(2)
            .map(Value::to_number)
            .map(|length| length.max(0.0) as usize)
            .unwrap_or(available)
            .min(available);
        return new_view(first.clone(), offset, length, kind);
    }
    if first.is_number() {
        let length = first.to_number().max(0.0) as usize;
        return new_view(new_buffer(vec![0; length * kind.bytes()]), 0, length, kind);
    }
    let values: Vec<Value> = first.iter().collect();
    let buffer = new_buffer(vec![0; values.len() * kind.bytes()]);
    let value = new_view(buffer, 0, values.len(), kind);
    for (index, item) in values.into_iter().enumerate() {
        set_typed_array_index(&value, index, item);
    }
    value
}

fn build_typed_array_classes() -> HashMap<String, Value> {
    TYPED_ARRAY_NAMES
        .iter()
        .map(|name| {
            let kind = Kind::from_name(name);
            let class = Value::function(move |_, args| construct_typed_array(kind, &args));
            class.set_property("name", Value::string(name));
            class.set_property("BYTES_PER_ELEMENT", Value::Number(kind.bytes() as f64));
            class.set_property("__w3cos_typed_array_name", Value::string(name));
            let prototype = Value::object(HashMap::new());
            prototype.set_property("constructor", class.clone());
            prototype.set_property("BYTES_PER_ELEMENT", Value::Number(kind.bytes() as f64));
            class.set_property("prototype", prototype);
            ((*name).to_string(), class)
        })
        .collect()
}

pub fn typed_array_class(name: &str) -> Value {
    CLASSES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_typed_array_classes());
        }
        slot.borrow()
            .as_ref()
            .and_then(|classes| classes.get(name))
            .cloned()
            .unwrap_or(Value::Undefined)
    })
}

pub fn typed_array_value(values: Vec<Value>) -> Value {
    construct_typed_array(Kind::Uint8, &[Value::array(values)])
}

pub fn is_typed_array(value: &Value) -> bool {
    view_for(value).is_some()
}

pub fn typed_array_property(value: &Value, key: &str) -> Option<Value> {
    let (bytes, offset, length, kind, buffer) = view_for(value)?;
    match key {
        "buffer" => Some(buffer),
        "byteOffset" => Some(Value::Number(offset as f64)),
        "byteLength" => Some(Value::Number((length * kind.bytes()) as f64)),
        "BYTES_PER_ELEMENT" => Some(Value::Number(kind.bytes() as f64)),
        "length" => Some(Value::Number(length as f64)),
        _ => {
            let index = key.parse::<usize>().ok()?;
            if index >= length {
                return Some(Value::Undefined);
            }
            let start = offset + index * kind.bytes();
            Some(kind.decode(&bytes.borrow()[start..start + kind.bytes()]))
        }
    }
}

pub fn set_typed_array_index(value: &Value, index: usize, item: Value) -> bool {
    let Some((bytes, offset, length, kind, _)) = view_for(value) else {
        return false;
    };
    if index >= length {
        return true;
    }
    let encoded = kind.encode(&item);
    let start = offset + index * kind.bytes();
    bytes.borrow_mut()[start..start + kind.bytes()].copy_from_slice(&encoded);
    sync_views(&bytes);
    true
}

pub fn typed_array_instance_of(value: &Value, class: &Value) -> bool {
    let Some((_, _, _, kind, _)) = view_for(value) else {
        return false;
    };
    class
        .get_property("__w3cos_typed_array_name")
        .to_js_string()
        == TYPED_ARRAY_NAMES
            .iter()
            .find(|name| Kind::from_name(name) == kind)
            .copied()
            .unwrap_or("")
}

fn binary_error(name: &str, message: &str) -> ! {
    crate::throw_value(Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ])))
}

fn atomic_location(args: &[Value]) -> (Rc<RefCell<Vec<u8>>>, usize, Kind) {
    let array = args.first().cloned().unwrap_or(Value::Undefined);
    let Some((bytes, offset, length, kind, buffer)) = view_for(&array) else {
        binary_error("TypeError", "Atomics requires an integer typed array");
    };
    if !kind.supports_atomics() {
        binary_error("TypeError", "Atomics requires an integer typed array");
    }
    if !is_shared_buffer(&buffer) {
        binary_error(
            "TypeError",
            "Atomics requires a SharedArrayBuffer-backed typed array",
        );
    }
    let index_number = args.get(1).map(Value::to_number).unwrap_or(0.0);
    if !index_number.is_finite() || index_number < 0.0 || index_number.fract() != 0.0 {
        binary_error("RangeError", "Atomics index is outside the typed array");
    }
    let index = index_number as usize;
    if index >= length {
        binary_error("RangeError", "Atomics index is outside the typed array");
    }
    (bytes, offset + index * kind.bytes(), kind)
}

fn raw_value(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .enumerate()
        .fold(0_u64, |value, (index, byte)| {
            value | (u64::from(*byte) << (index * 8))
        })
}

fn raw_bytes(value: u64, length: usize) -> Vec<u8> {
    value.to_le_bytes()[..length].to_vec()
}

fn atomic_operand(kind: Kind, value: &Value) -> Vec<u8> {
    let wants_bigint = matches!(kind, Kind::BigInt64 | Kind::BigUint64);
    if wants_bigint != crate::bigint::get(value).is_some() {
        binary_error(
            "TypeError",
            if wants_bigint {
                "BigInt typed arrays require BigInt operands"
            } else {
                "Numeric typed arrays require Number operands"
            },
        );
    }
    kind.encode(value)
}

fn atomic_load(args: Vec<Value>) -> Value {
    let (bytes, start, kind) = atomic_location(&args);
    let bytes = bytes.borrow();
    kind.decode(&bytes[start..start + kind.bytes()])
}

fn atomic_store(args: Vec<Value>) -> Value {
    let (bytes, start, kind) = atomic_location(&args);
    let value = args.get(2).cloned().unwrap_or(Value::Undefined);
    let encoded = atomic_operand(kind, &value);
    {
        bytes.borrow_mut()[start..start + kind.bytes()].copy_from_slice(&encoded);
    }
    sync_views(&bytes);
    kind.decode(&encoded)
}

#[derive(Clone, Copy)]
enum AtomicUpdate {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Exchange,
}

fn atomic_update(args: Vec<Value>, operation: AtomicUpdate) -> Value {
    let (bytes, start, kind) = atomic_location(&args);
    let operand = atomic_operand(kind, args.get(2).unwrap_or(&Value::Undefined));
    let (old, replacement) = {
        let bytes = bytes.borrow();
        let old = bytes[start..start + kind.bytes()].to_vec();
        let left = raw_value(&old);
        let right = raw_value(&operand);
        let replacement = match operation {
            AtomicUpdate::Add => left.wrapping_add(right),
            AtomicUpdate::Sub => left.wrapping_sub(right),
            AtomicUpdate::And => left & right,
            AtomicUpdate::Or => left | right,
            AtomicUpdate::Xor => left ^ right,
            AtomicUpdate::Exchange => right,
        };
        (old, raw_bytes(replacement, kind.bytes()))
    };
    {
        bytes.borrow_mut()[start..start + kind.bytes()].copy_from_slice(&replacement);
    }
    sync_views(&bytes);
    kind.decode(&old)
}

fn atomic_compare_exchange(args: Vec<Value>) -> Value {
    let (bytes, start, kind) = atomic_location(&args);
    let expected = atomic_operand(kind, args.get(2).unwrap_or(&Value::Undefined));
    let replacement = atomic_operand(kind, args.get(3).unwrap_or(&Value::Undefined));
    let old = {
        let mut bytes = bytes.borrow_mut();
        let old = bytes[start..start + kind.bytes()].to_vec();
        if old == expected {
            bytes[start..start + kind.bytes()].copy_from_slice(&replacement);
        }
        old
    };
    sync_views(&bytes);
    kind.decode(&old)
}

fn atomic_is_lock_free(args: Vec<Value>) -> Value {
    let size = args.first().map(Value::to_number).unwrap_or(0.0);
    Value::Bool(matches!(size as u64, 1 | 2 | 4 | 8) && size.fract() == 0.0)
}

fn atomic_wait_result(args: &[Value]) -> &'static str {
    let (bytes, start, kind) = atomic_location(args);
    if !kind.supports_wait() {
        binary_error(
            "TypeError",
            "Atomics.wait requires Int32Array or BigInt64Array",
        );
    }
    let expected = atomic_operand(kind, args.get(2).unwrap_or(&Value::Undefined));
    let bytes = bytes.borrow();
    if bytes[start..start + kind.bytes()] != expected {
        return "not-equal";
    }
    ATOMICS_WAIT_WARNING_EMITTED.with(|emitted| {
        if !std::mem::replace(&mut *emitted.borrow_mut(), true) {
            eprintln!(
                "warning: w3cos Atomics.wait uses non-blocking timed-out fallback on this host"
            );
        }
    });
    "timed-out"
}

fn atomic_wait(args: Vec<Value>) -> Value {
    Value::string(atomic_wait_result(&args))
}

fn atomic_wait_async(args: Vec<Value>) -> Value {
    Value::object(HashMap::from([
        ("async".into(), Value::Bool(false)),
        ("value".into(), Value::string(atomic_wait_result(&args))),
    ]))
}

fn atomic_notify(args: Vec<Value>) -> Value {
    let (_, _, kind) = atomic_location(&args);
    if !kind.supports_wait() {
        binary_error(
            "TypeError",
            "Atomics.notify requires Int32Array or BigInt64Array",
        );
    }
    Value::Number(0.0)
}

pub fn atomics_value() -> Value {
    ATOMICS_VALUE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([
            (
                "add".into(),
                Value::function(|_, args| atomic_update(args, AtomicUpdate::Add)),
            ),
            (
                "sub".into(),
                Value::function(|_, args| atomic_update(args, AtomicUpdate::Sub)),
            ),
            (
                "and".into(),
                Value::function(|_, args| atomic_update(args, AtomicUpdate::And)),
            ),
            (
                "or".into(),
                Value::function(|_, args| atomic_update(args, AtomicUpdate::Or)),
            ),
            (
                "xor".into(),
                Value::function(|_, args| atomic_update(args, AtomicUpdate::Xor)),
            ),
            (
                "exchange".into(),
                Value::function(|_, args| atomic_update(args, AtomicUpdate::Exchange)),
            ),
            (
                "compareExchange".into(),
                Value::function(|_, args| atomic_compare_exchange(args)),
            ),
            ("load".into(), Value::function(|_, args| atomic_load(args))),
            (
                "store".into(),
                Value::function(|_, args| atomic_store(args)),
            ),
            (
                "isLockFree".into(),
                Value::function(|_, args| atomic_is_lock_free(args)),
            ),
            ("wait".into(), Value::function(|_, args| atomic_wait(args))),
            (
                "waitAsync".into(),
                Value::function(|_, args| atomic_wait_async(args)),
            ),
            (
                "notify".into(),
                Value::function(|_, args| atomic_notify(args)),
            ),
        ]));
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn read_data(bytes: &[u8], offset: usize, kind: Kind, little: bool) -> Value {
    let end = offset + kind.bytes();
    if end > bytes.len() {
        return Value::Undefined;
    }
    let mut data = bytes[offset..end].to_vec();
    if !little && kind.bytes() > 1 {
        data.reverse();
    }
    kind.decode(&data)
}

fn write_data(
    bytes: &Rc<RefCell<Vec<u8>>>,
    offset: usize,
    kind: Kind,
    value: &Value,
    little: bool,
) {
    let mut encoded = kind.encode(value);
    if !little && kind.bytes() > 1 {
        encoded.reverse();
    }
    if offset + encoded.len() <= bytes.borrow().len() {
        bytes.borrow_mut()[offset..offset + encoded.len()].copy_from_slice(&encoded);
        sync_views(bytes);
    }
}

pub fn data_view_class() -> Value {
    DATA_VIEW_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let buffer = args
                .first()
                .cloned()
                .unwrap_or_else(|| new_buffer(Vec::new()));
            let Some(bytes) = buffer_state(&buffer) else {
                return Value::Undefined;
            };
            let offset = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
            let length = args
                .get(2)
                .map(Value::to_number)
                .map(|length| length.max(0.0) as usize)
                .unwrap_or_else(|| bytes.borrow().len().saturating_sub(offset))
                .min(bytes.borrow().len().saturating_sub(offset));
            let view = Value::object(HashMap::from([
                ("__w3cos_data_view".to_string(), Value::Bool(true)),
                ("buffer".to_string(), buffer),
                ("byteOffset".to_string(), Value::Number(offset as f64)),
                ("byteLength".to_string(), Value::Number(length as f64)),
            ]));
            for (name, kind) in [
                ("getUint8", Kind::Uint8),
                ("getInt8", Kind::Int8),
                ("getUint16", Kind::Uint16),
                ("getInt16", Kind::Int16),
                ("getUint32", Kind::Uint32),
                ("getInt32", Kind::Int32),
                ("getFloat32", Kind::Float32),
                ("getFloat64", Kind::Float64),
                ("getBigInt64", Kind::BigInt64),
                ("getBigUint64", Kind::BigUint64),
            ] {
                let bytes = bytes.clone();
                view.set_property(
                    name,
                    Value::function(move |_, args| {
                        let index =
                            args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                        let little = args.get(1).is_some_and(Value::to_bool);
                        read_data(&bytes.borrow(), offset + index, kind, little)
                    }),
                );
            }
            for (name, kind) in [
                ("setUint8", Kind::Uint8),
                ("setInt8", Kind::Int8),
                ("setUint16", Kind::Uint16),
                ("setInt16", Kind::Int16),
                ("setUint32", Kind::Uint32),
                ("setInt32", Kind::Int32),
                ("setFloat32", Kind::Float32),
                ("setFloat64", Kind::Float64),
                ("setBigInt64", Kind::BigInt64),
                ("setBigUint64", Kind::BigUint64),
            ] {
                let bytes = bytes.clone();
                view.set_property(
                    name,
                    Value::function(move |_, args| {
                        let index =
                            args.first().map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
                        let value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
                        let little = args.get(2).is_some_and(Value::to_bool);
                        write_data(&bytes, offset + index, kind, &value, little);
                        Value::Undefined
                    }),
                );
            }
            let prototype = data_view_class().get_property("prototype");
            crate::class::set_prototype_of(&view, &prototype);
            view
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PanicValue;

    #[test]
    fn typed_arrays_and_data_view_share_array_buffer_storage() {
        let buffer = crate::class::construct(&array_buffer_class(), vec![Value::Number(8.0)]);
        let bytes = crate::class::construct(&typed_array_class("Uint8Array"), vec![buffer.clone()]);
        let words = crate::class::construct(
            &typed_array_class("Uint16Array"),
            vec![buffer.clone(), Value::Number(2.0), Value::Number(2.0)],
        );
        bytes.set_property("2", Value::Number(0x34 as f64));
        bytes.set_property("3", Value::Number(0x12 as f64));
        assert_eq!(words.get_property("0").to_number(), 0x1234 as f64);
        assert!(bytes.get_property("buffer").strict_eq(&buffer));
        assert_eq!(words.get_property("byteOffset").to_number(), 2.0);
        assert_eq!(words.get_property("byteLength").to_number(), 4.0);

        let view = crate::class::construct(&data_view_class(), vec![buffer]);
        view.call_method(
            "setUint16",
            vec![
                Value::Number(4.0),
                Value::Number(0xabcd as f64),
                Value::Bool(false),
            ],
        );
        assert_eq!(bytes.get_property("4").to_number(), 0xab as f64);
        assert_eq!(bytes.get_property("5").to_number(), 0xcd as f64);
        assert_eq!(
            view.call_method("getUint16", vec![Value::Number(4.0), Value::Bool(false)])
                .to_number(),
            0xabcd as f64
        );
    }

    #[test]
    fn bigint_views_read_and_write_bigint_values() {
        let buffer = crate::class::construct(&array_buffer_class(), vec![Value::Number(8.0)]);
        let signed =
            crate::class::construct(&typed_array_class("BigInt64Array"), vec![buffer.clone()]);
        signed.set_property("0", crate::bigint::parse("-2"));
        assert_eq!(signed.get_property("0").type_of(), "bigint");
        assert_eq!(signed.get_property("0").to_js_string(), "-2");

        let view = crate::class::construct(&data_view_class(), vec![buffer]);
        assert_eq!(
            view.call_method("getBigInt64", vec![Value::Number(0.0), Value::Bool(true)])
                .to_js_string(),
            "-2"
        );
        view.call_method(
            "setBigUint64",
            vec![
                Value::Number(0.0),
                crate::bigint::parse("18446744073709551615"),
                Value::Bool(true),
            ],
        );
        assert_eq!(
            view.call_method("getBigUint64", vec![Value::Number(0.0), Value::Bool(true)])
                .to_js_string(),
            "18446744073709551615"
        );
    }

    #[test]
    fn shared_array_buffer_supports_atomic_integer_operations() {
        let buffer =
            crate::class::construct(&shared_array_buffer_class(), vec![Value::Number(16.0)]);
        let words = crate::class::construct(&typed_array_class("Int32Array"), vec![buffer.clone()]);
        let atomics = atomics_value();

        assert_eq!(
            atomics.call_method(
                "store",
                vec![words.clone(), Value::Number(0.0), Value::Number(5.0)],
            ),
            Value::Number(5.0)
        );
        assert_eq!(
            atomics.call_method(
                "add",
                vec![words.clone(), Value::Number(0.0), Value::Number(3.0)],
            ),
            Value::Number(5.0)
        );
        assert_eq!(
            atomics.call_method("load", vec![words.clone(), Value::Number(0.0)]),
            Value::Number(8.0)
        );
        assert_eq!(
            atomics.call_method(
                "compareExchange",
                vec![
                    words.clone(),
                    Value::Number(0.0),
                    Value::Number(8.0),
                    Value::Number(11.0),
                ],
            ),
            Value::Number(8.0)
        );
        assert_eq!(
            atomics
                .call_method(
                    "wait",
                    vec![
                        words.clone(),
                        Value::Number(0.0),
                        Value::Number(99.0),
                        Value::Number(0.0),
                    ],
                )
                .to_js_string(),
            "not-equal"
        );
        let wait = atomics.call_method(
            "waitAsync",
            vec![
                words.clone(),
                Value::Number(0.0),
                Value::Number(11.0),
                Value::Number(0.0),
            ],
        );
        assert_eq!(wait.get_property("async"), Value::Bool(false));
        assert_eq!(wait.get_property("value").to_js_string(), "timed-out");
        assert_eq!(
            atomics.call_method("notify", vec![words, Value::Number(0.0)]),
            Value::Number(0.0)
        );
        assert!(crate::class::instance_of(
            &buffer,
            &shared_array_buffer_class()
        ));
    }

    #[test]
    fn atomics_wraps_integer_width_and_supports_bigint_arrays() {
        let byte_buffer =
            crate::class::construct(&shared_array_buffer_class(), vec![Value::Number(1.0)]);
        let bytes = crate::class::construct(&typed_array_class("Int8Array"), vec![byte_buffer]);
        bytes.set_property("0", Value::Number(127.0));
        assert_eq!(
            atomics_value().call_method(
                "add",
                vec![bytes.clone(), Value::Number(0.0), Value::Number(1.0)],
            ),
            Value::Number(127.0)
        );
        assert_eq!(bytes.get_property("0"), Value::Number(-128.0));

        let big_buffer =
            crate::class::construct(&shared_array_buffer_class(), vec![Value::Number(8.0)]);
        let big = crate::class::construct(&typed_array_class("BigInt64Array"), vec![big_buffer]);
        atomics_value().call_method(
            "store",
            vec![big.clone(), Value::Number(0.0), crate::bigint::parse("5")],
        );
        let previous = atomics_value().call_method(
            "add",
            vec![big.clone(), Value::Number(0.0), crate::bigint::parse("2")],
        );
        assert_eq!(previous.to_js_string(), "5");
        assert_eq!(big.get_property("0").to_js_string(), "7");
    }

    #[test]
    fn atomics_rejects_non_shared_typed_arrays() {
        let array =
            crate::class::construct(&typed_array_class("Int32Array"), vec![Value::Number(1.0)]);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            atomics_value().call_method("load", vec![array, Value::Number(0.0)])
        }));
        let error = outcome
            .expect_err("ordinary ArrayBuffer should not be accepted")
            .downcast::<PanicValue>()
            .expect("exception should contain a JavaScript value");
        assert_eq!(error.0.get_property("name").to_js_string(), "TypeError");
    }
}
