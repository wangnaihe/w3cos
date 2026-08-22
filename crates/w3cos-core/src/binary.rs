//! ArrayBuffer, DataView, and typed-array backing-store semantics.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::Value;
use crate::value::{ValueIterator, WeakJsArray};

const BUFFER_STATE_KEY: &str = "__w3cos_array_buffer_id";
const SHARED_BUFFER_KEY: &str = "__w3cos_shared_array_buffer";
const MAX_BYTE_LENGTH_KEY: &str = "__w3cos_max_byte_length";
const RESIZABLE_BUFFER_KEY: &str = "__w3cos_resizable_array_buffer";
const DETACHED_BUFFER_KEY: &str = "__w3cos_detached_array_buffer";

pub const TYPED_ARRAY_NAMES: &[&str] = &[
    "Uint8Array",
    "Uint8ClampedArray",
    "Int8Array",
    "Uint16Array",
    "Int16Array",
    "Uint32Array",
    "Int32Array",
    "Float16Array",
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
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

fn f16_bits_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;
    match exponent {
        0 if fraction == 0 => f32::from_bits(sign),
        0 => {
            let value = f32::from(fraction) * 2.0_f32.powi(-24);
            if sign == 0 { value } else { -value }
        }
        0x1f => f32::from_bits(sign | 0x7f80_0000 | (u32::from(fraction) << 13)),
        _ => f32::from_bits(
            sign | (u32::from(exponent + (127 - 15)) << 23) | (u32::from(fraction) << 13),
        ),
    }
}

fn round_shift_to_even(value: u64, shift: u32) -> u64 {
    let truncated = value >> shift;
    let remainder = value & ((1_u64 << shift) - 1);
    let halfway = 1_u64 << (shift - 1);
    truncated + u64::from(remainder > halfway || (remainder == halfway && truncated & 1 != 0))
}

fn f64_to_f16_bits(value: f64) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    if exponent == 0x7ff {
        return if fraction == 0 {
            sign | 0x7c00
        } else {
            sign | 0x7e00
        };
    }
    if exponent == 0 {
        return sign;
    }
    let unbiased = exponent - 1023;
    let half_exponent = unbiased + 15;
    if half_exponent >= 0x1f {
        return sign | 0x7c00;
    }
    if half_exponent <= 0 {
        if unbiased < -25 {
            return sign;
        }
        let significand = fraction | (1_u64 << 52);
        let rounded = round_shift_to_even(significand, (28 - unbiased) as u32);
        return sign | rounded as u16;
    }
    let mut exponent_bits = half_exponent as u16;
    let significand = fraction | (1_u64 << 52);
    let rounded = round_shift_to_even(significand, 42);
    let mut fraction_bits = (rounded & 0x03ff) as u16;
    if rounded == 0x0800 {
        fraction_bits = 0;
        exponent_bits += 1;
        if exponent_bits == 0x1f {
            return sign | 0x7c00;
        }
    }
    sign | (exponent_bits << 10) | fraction_bits
}

pub fn f16_round(value: f64) -> f64 {
    f16_bits_to_f32(f64_to_f16_bits(value)) as f64
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
            "Float16Array" => Self::Float16,
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
            Self::Uint16 | Self::Int16 | Self::Float16 => 2,
            Self::Uint32 | Self::Int32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Uint8 => "Uint8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int8 => "Int8Array",
            Self::Uint16 => "Uint16Array",
            Self::Int16 => "Int16Array",
            Self::Uint32 => "Uint32Array",
            Self::Int32 => "Int32Array",
            Self::Float16 => "Float16Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
            Self::BigInt64 => "BigInt64Array",
            Self::BigUint64 => "BigUint64Array",
        }
    }

    fn supports_atomics(self) -> bool {
        !matches!(
            self,
            Self::Uint8Clamped | Self::Float16 | Self::Float32 | Self::Float64
        )
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
            Self::Float16 => f16_bits_to_f32(u16::from_le_bytes([bytes[0], bytes[1]])) as f64,
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
            Self::Float16 => f64_to_f16_bits(number).to_le_bytes().to_vec(),
            Self::Float32 => (number as f32).to_le_bytes().to_vec(),
            Self::Float64 => number.to_le_bytes().to_vec(),
            Self::BigInt64 | Self::BigUint64 => unreachable!("handled above"),
        }
    }
}

struct View {
    storage: WeakJsArray,
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
    static RESIZABLE_VIEW_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
}

fn buffer_state(value: &Value) -> Option<Rc<RefCell<Vec<u8>>>> {
    let Some(object) = value.as_object() else {
        return None;
    };
    let Some(id) = object.borrow().get_direct(BUFFER_STATE_KEY).as_number() else {
        return None;
    };
    BUFFERS.with(|buffers| buffers.borrow().get(&(id as u64)).cloned())
}

fn new_buffer_from_state_with_options(
    state: Rc<RefCell<Vec<u8>>>,
    shared: bool,
    max_byte_length: usize,
    resizable: bool,
) -> Value {
    let id = NEXT_BUFFER_ID.with(|next| {
        let id = *next.borrow();
        *next.borrow_mut() = id + 1;
        id
    });
    BUFFERS.with(|buffers| buffers.borrow_mut().insert(id, state.clone()));
    let value = Value::object(HashMap::from([
        (BUFFER_STATE_KEY.to_string(), Value::Number(id as f64)),
        (SHARED_BUFFER_KEY.to_string(), Value::Bool(shared)),
        (
            MAX_BYTE_LENGTH_KEY.to_string(),
            Value::Number(max_byte_length as f64),
        ),
        (RESIZABLE_BUFFER_KEY.to_string(), Value::Bool(resizable)),
        (DETACHED_BUFFER_KEY.to_string(), Value::Bool(false)),
    ]));
    let state_for_length = state.clone();
    value.set_property(
        "__w3cos_getter_byteLength",
        Value::function(move |_, _| Value::Number(state_for_length.borrow().len() as f64)),
    );
    let value_for_maximum = value.clone();
    value.set_property(
        "__w3cos_getter_maxByteLength",
        Value::function(move |_, _| {
            Value::Number(if value_for_maximum
                .get_property(DETACHED_BUFFER_KEY)
                .to_bool()
            {
                0
            } else {
                max_byte_length
            } as f64)
        }),
    );
    let value_for_resizable = value.clone();
    value.set_property(
        if shared {
            "__w3cos_getter_growable"
        } else {
            "__w3cos_getter_resizable"
        },
        Value::function(move |_, _| {
            Value::Bool(
                resizable
                    && !value_for_resizable
                        .get_property(DETACHED_BUFFER_KEY)
                        .to_bool(),
            )
        }),
    );
    if !shared {
        let value_for_detached = value.clone();
        value.set_property(
            "__w3cos_getter_detached",
            Value::function(move |_, _| {
                Value::Bool(
                    value_for_detached
                        .get_property(DETACHED_BUFFER_KEY)
                        .to_bool(),
                )
            }),
        );
    }
    let value_for_slice = value.clone();
    value.set_property(
        "slice",
        Value::function(move |_, args| {
            if value_for_slice.get_property(DETACHED_BUFFER_KEY).to_bool() {
                binary_error("TypeError", "ArrayBuffer is detached");
            }
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
    if shared {
        let state_for_grow = state;
        value.set_property(
            "grow",
            Value::function(move |_, args| {
                if !resizable {
                    binary_error("TypeError", "SharedArrayBuffer is not growable");
                }
                let length = to_index(
                    args.first(),
                    0,
                    "SharedArrayBuffer length is outside the supported range",
                );
                let current = state_for_grow.borrow().len();
                if length < current || length > max_byte_length {
                    binary_error(
                        "RangeError",
                        "SharedArrayBuffer length is outside its growth range",
                    );
                }
                state_for_grow.borrow_mut().resize(length, 0);
                sync_views(&state_for_grow);
                Value::Undefined
            }),
        );
    } else {
        let state_for_resize = state.clone();
        let value_for_resize = value.clone();
        value.set_property(
            "resize",
            Value::function(move |_, args| {
                if value_for_resize.get_property(DETACHED_BUFFER_KEY).to_bool() {
                    binary_error("TypeError", "ArrayBuffer is detached");
                }
                if !resizable {
                    binary_error("TypeError", "ArrayBuffer is not resizable");
                }
                let length = to_index(
                    args.first(),
                    0,
                    "ArrayBuffer length is outside the supported range",
                );
                if length > max_byte_length {
                    binary_error("RangeError", "ArrayBuffer exceeds its maxByteLength");
                }
                state_for_resize.borrow_mut().resize(length, 0);
                sync_views(&state_for_resize);
                Value::Undefined
            }),
        );
        for (name, fixed) in [("transfer", false), ("transferToFixedLength", true)] {
            let source = value.clone();
            value.set_property(
                name,
                Value::function(move |_, args| {
                    if source.get_property(DETACHED_BUFFER_KEY).to_bool() {
                        binary_error("TypeError", "ArrayBuffer is detached");
                    }
                    let bytes = buffer_state(&source).expect("registered buffer");
                    let current = bytes.borrow().len();
                    let length = to_index(
                        args.first(),
                        current,
                        "ArrayBuffer transfer length is outside the supported range",
                    );
                    if !fixed && resizable && length > max_byte_length {
                        binary_error("RangeError", "ArrayBuffer exceeds its maxByteLength");
                    }
                    let mut contents = bytes.borrow().clone();
                    contents.resize(length, 0);
                    let target = if fixed || !resizable {
                        new_buffer(contents)
                    } else {
                        new_resizable_buffer(contents, max_byte_length, false)
                    };
                    detach_array_buffer(&source);
                    target
                }),
            );
        }
    }
    value
}

fn new_buffer_from_state(state: Rc<RefCell<Vec<u8>>>, shared: bool) -> Value {
    let length = state.borrow().len();
    new_buffer_from_state_with_options(state, shared, length, false)
}

fn new_buffer_with_shared(bytes: Vec<u8>, shared: bool) -> Value {
    new_buffer_from_state(Rc::new(RefCell::new(bytes)), shared)
}

fn new_resizable_buffer(bytes: Vec<u8>, max_byte_length: usize, shared: bool) -> Value {
    let value = new_buffer_from_state_with_options(
        Rc::new(RefCell::new(bytes)),
        shared,
        max_byte_length,
        true,
    );
    let prototype = if shared {
        shared_array_buffer_class().get_property("prototype")
    } else {
        array_buffer_class().get_property("prototype")
    };
    crate::class::set_prototype_of(&value, &prototype);
    value
}

fn new_buffer(bytes: Vec<u8>) -> Value {
    let value = new_buffer_with_shared(bytes, false);
    crate::class::set_prototype_of(&value, &array_buffer_class().get_property("prototype"));
    value
}

pub fn array_buffer_value(bytes: Vec<u8>) -> Value {
    new_buffer(bytes)
}

pub fn shared_array_buffer_value(bytes: Vec<u8>) -> Value {
    let value = new_buffer_with_shared(bytes, true);
    crate::class::set_prototype_of(
        &value,
        &shared_array_buffer_class().get_property("prototype"),
    );
    value
}

fn is_shared_buffer(value: &Value) -> bool {
    value.get_property(SHARED_BUFFER_KEY).to_bool()
}

fn is_resizable_buffer(value: &Value) -> bool {
    value.get_property(RESIZABLE_BUFFER_KEY).to_bool()
}

fn max_byte_length(value: &Value) -> usize {
    value.get_property(MAX_BYTE_LENGTH_KEY).to_number().max(0.0) as usize
}

fn warn_resizable_view_fallback() {
    RESIZABLE_VIEW_WARNING_EMITTED.with(|emitted| {
        if !std::mem::replace(&mut *emitted.borrow_mut(), true) {
            eprintln!(
                "warning: w3cos resizable-buffer views retain their construction-time length; \
                 automatic length tracking is not yet available"
            );
        }
    });
}

pub fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    if let Some(bytes) = buffer_state(value) {
        return Some(bytes.borrow().clone());
    }
    let (bytes, offset, length, kind, _) = view_for(value)?;
    let end = offset + length * kind.bytes();
    if end > bytes.borrow().len() {
        return Some(Vec::new());
    }
    Some(bytes.borrow()[offset..end].to_vec())
}

/// Returns true when `value` is a TypedArray or DataView.
pub fn is_array_buffer_view(value: &Value) -> bool {
    view_descriptor(value).is_some()
}

/// Byte range of a TypedArray or DataView within its backing ArrayBuffer.
pub fn array_buffer_view_range(value: &Value) -> Option<(Value, usize, usize)> {
    let (buffer, offset, byte_length, _) = view_descriptor(value)?;
    Some((buffer, offset, byte_length))
}

/// Copy `source` into `view` and return a same-kind view over the written prefix.
///
/// The returned view shares `view`'s ArrayBuffer. Extra source bytes are not
/// copied. Multi-byte TypedArrays keep only a whole-element prefix.
pub fn fill_array_buffer_view(view: &Value, source: &[u8]) -> Option<Value> {
    let (buffer, offset, byte_length, kind) = view_descriptor(view)?;
    let bytes = buffer_state(&buffer)?;
    if offset
        .checked_add(byte_length)
        .is_none_or(|end| end > bytes.borrow().len())
    {
        return None;
    }
    let written = match kind {
        Some(kind) => {
            let aligned = source.len().min(byte_length);
            aligned - (aligned % kind.bytes())
        }
        None => source.len().min(byte_length),
    };
    bytes.borrow_mut()[offset..offset + written].copy_from_slice(&source[..written]);
    sync_views(&bytes);
    Some(slice_array_buffer_view(view, written).unwrap_or_else(|| view.clone()))
}

/// Return a same-kind TypedArray or DataView covering the first `byte_length`
/// bytes of `view`. The result shares the original ArrayBuffer.
pub fn slice_array_buffer_view(view: &Value, byte_length: usize) -> Option<Value> {
    let (buffer, offset, capacity, kind) = view_descriptor(view)?;
    if byte_length > capacity {
        return None;
    }
    match kind {
        Some(kind) => {
            if byte_length % kind.bytes() != 0 {
                return None;
            }
            Some(new_view(buffer, offset, byte_length / kind.bytes(), kind))
        }
        None => Some(crate::class::construct(
            &data_view_class(),
            vec![
                buffer,
                Value::Number(offset as f64),
                Value::Number(byte_length as f64),
            ],
        )),
    }
}

fn view_descriptor(value: &Value) -> Option<(Value, usize, usize, Option<Kind>)> {
    if let Some((_, offset, length, kind, buffer)) = view_for(value) {
        return Some((buffer, offset, length * kind.bytes(), Some(kind)));
    }
    if !value.get_property("__w3cos_data_view").to_bool() {
        return None;
    }
    let buffer = value.get_property("buffer");
    buffer_state(&buffer)?;
    let offset = value.get_property("byteOffset").to_number().max(0.0) as usize;
    let byte_length = value.get_property("byteLength").to_number().max(0.0) as usize;
    Some((buffer, offset, byte_length, None))
}

pub enum BinaryCloneDescriptor {
    ArrayBuffer {
        shared: bool,
    },
    TypedArray {
        name: &'static str,
        buffer: Value,
        offset: usize,
        length: usize,
    },
    DataView {
        buffer: Value,
        offset: usize,
        length: usize,
    },
}

pub fn clone_descriptor(value: &Value) -> Option<BinaryCloneDescriptor> {
    if buffer_state(value).is_some() {
        return Some(BinaryCloneDescriptor::ArrayBuffer {
            shared: is_shared_buffer(value),
        });
    }
    if let Some((_, offset, length, kind, buffer)) = view_for(value) {
        return Some(BinaryCloneDescriptor::TypedArray {
            name: kind.name(),
            buffer,
            offset,
            length,
        });
    }
    value
        .get_property("__w3cos_data_view")
        .to_bool()
        .then(|| BinaryCloneDescriptor::DataView {
            buffer: value.get_property("buffer"),
            offset: value.get_property("byteOffset").to_number().max(0.0) as usize,
            length: value.get_property("byteLength").to_number().max(0.0) as usize,
        })
}

pub(crate) fn clone_array_buffer(value: &Value, shared: bool) -> Value {
    let state = buffer_state(value).expect("ArrayBuffer descriptor must have backing state");
    let resizable = is_resizable_buffer(value);
    let maximum = max_byte_length(value);
    if shared {
        let cloned = new_buffer_from_state_with_options(state, true, maximum, resizable);
        crate::class::set_prototype_of(
            &cloned,
            &shared_array_buffer_class().get_property("prototype"),
        );
        cloned
    } else if resizable {
        new_resizable_buffer(state.borrow().clone(), maximum, false)
    } else {
        new_buffer(state.borrow().clone())
    }
}

pub(crate) fn is_transferable_array_buffer(value: &Value) -> bool {
    buffer_state(value).is_some() && !is_shared_buffer(value)
}

pub(crate) fn detach_array_buffer(value: &Value) {
    let Some(state) = buffer_state(value) else {
        return;
    };
    state.borrow_mut().clear();
    value.set_property(DETACHED_BUFFER_KEY, Value::Bool(true));
    VIEWS.with(|views| {
        for view in views.borrow_mut().iter_mut() {
            if !Rc::ptr_eq(&view.bytes, &state) {
                continue;
            }
            view.length = 0;
            if let Some(storage) = view.storage.upgrade() {
                storage.borrow_mut().clear();
            }
        }
    });
}

fn normalize_index(value: Option<&Value>, length: usize, fallback: usize) -> usize {
    let number = value.map(Value::to_number).unwrap_or(fallback as f64);
    if number.is_sign_negative() {
        (length as i64 + number as i64).max(0) as usize
    } else {
        (number.max(0.0) as usize).min(length)
    }
}

fn to_index(value: Option<&Value>, fallback: usize, message: &str) -> usize {
    let Some(value) = value.filter(|value| !value.is_undefined()) else {
        return fallback;
    };
    let number = value.to_number();
    if number.is_nan() || number == 0.0 {
        return 0;
    }
    if !number.is_finite() || number < 0.0 || number > 9_007_199_254_740_991.0 {
        binary_error("RangeError", message);
    }
    number.trunc() as usize
}

pub fn array_buffer_class() -> Value {
    ARRAY_BUFFER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let length = to_index(
                args.first(),
                0,
                "ArrayBuffer length is outside the supported range",
            );
            let maximum = args
                .get(1)
                .map(|options| options.get_property("maxByteLength"))
                .filter(|value| !value.is_undefined())
                .map(|value| {
                    to_index(
                        Some(&value),
                        length,
                        "ArrayBuffer maxByteLength is outside the supported range",
                    )
                });
            if maximum.is_some_and(|maximum| maximum < length) {
                binary_error(
                    "RangeError",
                    "ArrayBuffer maxByteLength is smaller than byteLength",
                );
            }
            maximum.map_or_else(
                || new_buffer(vec![0; length]),
                |maximum| new_resizable_buffer(vec![0; length], maximum, false),
            )
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
            let length = to_index(
                args.first(),
                0,
                "SharedArrayBuffer length is outside the supported range",
            );
            let maximum = args
                .get(1)
                .map(|options| options.get_property("maxByteLength"))
                .filter(|value| !value.is_undefined())
                .map(|value| {
                    to_index(
                        Some(&value),
                        length,
                        "SharedArrayBuffer maxByteLength is outside the supported range",
                    )
                });
            if maximum.is_some_and(|maximum| maximum < length) {
                binary_error(
                    "RangeError",
                    "SharedArrayBuffer maxByteLength is smaller than byteLength",
                );
            }
            let buffer = maximum.map_or_else(
                || new_buffer_with_shared(vec![0; length], true),
                |maximum| new_resizable_buffer(vec![0; length], maximum, true),
            );
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
    let Some(candidate) = value.as_array() else {
        return None;
    };
    VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        views.retain(|view| view.storage.upgrade().is_some());
        views.iter().find_map(|view| {
            let storage = view.storage.upgrade()?;
            storage.ptr_eq(&candidate).then(|| {
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
        views.retain(|view| view.storage.upgrade().is_some());
        let bytes = changed.borrow();
        for view in views.iter() {
            if !Rc::ptr_eq(&view.bytes, changed) {
                continue;
            }
            let Some(storage) = view.storage.upgrade() else {
                continue;
            };
            let mut decoded = Vec::with_capacity(view.length);
            for index in 0..view.length {
                let start = view.offset + index * view.kind.bytes();
                let end = start + view.kind.bytes();
                if end <= bytes.len() {
                    decoded.push(view.kind.decode(&bytes[start..end]));
                }
            }
            storage.borrow_mut().replace_values(decoded);
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
    let storage = value.as_array().expect("typed array storage");
    VIEWS.with(|views| {
        views.borrow_mut().push(View {
            storage: storage.downgrade(),
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
        let offset = to_index(
            args.get(1),
            0,
            "TypedArray byteOffset is outside the buffer",
        );
        if offset % kind.bytes() != 0 {
            binary_error(
                "RangeError",
                "TypedArray byteOffset must align to the element size",
            );
        }
        let byte_length = bytes.borrow().len();
        if offset > byte_length {
            binary_error("RangeError", "TypedArray byteOffset is outside the buffer");
        }
        let remaining = byte_length - offset;
        if is_resizable_buffer(first) && args.get(2).is_none_or(Value::is_undefined) {
            warn_resizable_view_fallback();
        }
        let length = if args.get(2).is_some_and(|value| !value.is_undefined()) {
            let length = to_index(args.get(2), 0, "TypedArray length is outside the buffer");
            if length
                .checked_mul(kind.bytes())
                .is_none_or(|bytes| bytes > remaining)
            {
                binary_error("RangeError", "TypedArray length is outside the buffer");
            }
            length
        } else {
            if remaining % kind.bytes() != 0 {
                binary_error(
                    "RangeError",
                    "TypedArray buffer remainder must align to the element size",
                );
            }
            remaining / kind.bytes()
        };
        return new_view(first.clone(), offset, length, kind);
    }
    if first.is_number() {
        let length = to_index(
            Some(first),
            0,
            "TypedArray length is outside the supported range",
        );
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

fn typed_array_values(value: &Value) -> Option<Vec<Value>> {
    let (_, _, length, _, _) = view_for(value)?;
    Some(
        (0..length)
            .map(|index| {
                typed_array_property(value, &index.to_string()).unwrap_or(Value::Undefined)
            })
            .collect(),
    )
}

fn typed_array_from_values(kind: Kind, values: Vec<Value>) -> Value {
    let buffer = new_buffer(vec![0; values.len() * kind.bytes()]);
    let target = new_view(buffer, 0, values.len(), kind);
    for (index, value) in values.into_iter().enumerate() {
        set_typed_array_index(&target, index, value);
    }
    target
}

#[derive(Clone, Copy)]
enum TypedArrayIterationKind {
    Keys,
    Values,
    Entries,
}

struct TypedArrayIterator {
    value: Value,
    index: usize,
    kind: TypedArrayIterationKind,
}

impl Iterator for TypedArrayIterator {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        let (_, _, length, _, _) = view_for(&self.value)?;
        if self.index >= length {
            return None;
        }
        let index = self.index;
        self.index += 1;
        let value = match self.kind {
            TypedArrayIterationKind::Keys => Value::from(index),
            TypedArrayIterationKind::Values => {
                typed_array_property(&self.value, &index.to_string()).unwrap_or(Value::Undefined)
            }
            TypedArrayIterationKind::Entries => Value::array(vec![
                Value::from(index),
                typed_array_property(&self.value, &index.to_string()).unwrap_or(Value::Undefined),
            ]),
        };
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = view_for(&self.value)
            .map(|(_, _, length, _, _)| length.saturating_sub(self.index))
            .unwrap_or_default();
        (remaining, Some(remaining))
    }
}

fn typed_array_iterator(value: &Value, kind: TypedArrayIterationKind) -> Value {
    crate::value::iterator_object(ValueIterator::new(TypedArrayIterator {
        value: value.clone(),
        index: 0,
        kind,
    }))
}

pub(crate) fn typed_array_value_iterator(value: &Value) -> Option<ValueIterator> {
    view_for(value)?;
    Some(ValueIterator::new(TypedArrayIterator {
        value: value.clone(),
        index: 0,
        kind: TypedArrayIterationKind::Values,
    }))
}

fn typed_array_sort_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    if let Some(order) = crate::bigint::compare(left, right) {
        return order;
    }
    let left = left.to_number();
    let right = right.to_number();
    match (left.is_nan(), right.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) if left == 0.0 && right == 0.0 => {
            right.is_sign_negative().cmp(&left.is_sign_negative())
        }
        (false, false) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
    }
}

pub(crate) fn typed_array_call_method(value: &Value, key: &str, args: Vec<Value>) -> Option<Value> {
    let (_, offset, length, kind, buffer) = view_for(value)?;
    let argument = |index: usize| args.get(index).cloned().unwrap_or(Value::Undefined);
    let values = || typed_array_values(value).unwrap_or_default();
    Some(match key {
        "set" => {
            let source = argument(0).iter().collect::<Vec<_>>();
            let number = args.get(1).map(Value::to_number).unwrap_or(0.0);
            if !number.is_finite() || number < 0.0 {
                binary_error("RangeError", "TypedArray set offset is outside the target");
            }
            let target = number.trunc() as usize;
            if target
                .checked_add(source.len())
                .is_none_or(|end| end > length)
            {
                binary_error("RangeError", "TypedArray source does not fit in the target");
            }
            for (index, item) in source.into_iter().enumerate() {
                set_typed_array_index(value, target + index, item);
            }
            Value::Undefined
        }
        "subarray" => {
            let start = normalize_index(args.first(), length, 0);
            let end = normalize_index(args.get(1), length, length).max(start);
            new_view(buffer, offset + start * kind.bytes(), end - start, kind)
        }
        "slice" => {
            let snapshot = values();
            let start = normalize_index(args.first(), length, 0);
            let end = normalize_index(args.get(1), length, length).max(start);
            typed_array_from_values(kind, snapshot[start..end].to_vec())
        }
        "fill" => {
            let start = normalize_index(args.get(1), length, 0);
            let end = normalize_index(args.get(2), length, length).max(start);
            let item = argument(0);
            for index in start..end {
                set_typed_array_index(value, index, item.clone());
            }
            value.clone()
        }
        "copyWithin" => {
            let target = normalize_index(args.first(), length, 0);
            let start = normalize_index(args.get(1), length, 0);
            let end = normalize_index(args.get(2), length, length).max(start);
            let count = (end - start).min(length.saturating_sub(target));
            let source = values()[start..start + count].to_vec();
            for (index, item) in source.into_iter().enumerate() {
                set_typed_array_index(value, target + index, item);
            }
            value.clone()
        }
        "reverse" => {
            let mut snapshot = values();
            snapshot.reverse();
            for (index, item) in snapshot.into_iter().enumerate() {
                set_typed_array_index(value, index, item);
            }
            value.clone()
        }
        "sort" => {
            let comparator = args.first().cloned().unwrap_or(Value::Undefined);
            let mut snapshot = values();
            snapshot.sort_by(|left, right| {
                if comparator.is_function() {
                    let result = comparator
                        .call(Value::Undefined, vec![left.clone(), right.clone()])
                        .to_number();
                    if result.is_nan() || result == 0.0 {
                        std::cmp::Ordering::Equal
                    } else if result < 0.0 {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    }
                } else {
                    typed_array_sort_order(left, right)
                }
            });
            for (index, item) in snapshot.into_iter().enumerate() {
                set_typed_array_index(value, index, item);
            }
            value.clone()
        }
        "map" => {
            let callback = argument(0);
            typed_array_from_values(
                kind,
                values()
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        callback.call(
                            Value::Undefined,
                            vec![item, Value::Number(index as f64), value.clone()],
                        )
                    })
                    .collect(),
            )
        }
        "filter" => {
            let callback = argument(0);
            typed_array_from_values(
                kind,
                values()
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        callback
                            .call(
                                Value::Undefined,
                                vec![item.clone(), Value::Number(index as f64), value.clone()],
                            )
                            .to_bool()
                            .then_some(item)
                    })
                    .collect(),
            )
        }
        "keys" => typed_array_iterator(value, TypedArrayIterationKind::Keys),
        "values" => typed_array_iterator(value, TypedArrayIterationKind::Values),
        "entries" => typed_array_iterator(value, TypedArrayIterationKind::Entries),
        "toReversed" => {
            let mut snapshot = values();
            snapshot.reverse();
            typed_array_from_values(kind, snapshot)
        }
        "toSorted" => {
            let comparator = args.first().cloned().unwrap_or(Value::Undefined);
            let mut snapshot = values();
            snapshot.sort_by(|left, right| {
                if comparator.is_function() {
                    let result = comparator
                        .call(Value::Undefined, vec![left.clone(), right.clone()])
                        .to_number();
                    if result.is_nan() || result == 0.0 {
                        std::cmp::Ordering::Equal
                    } else if result < 0.0 {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    }
                } else {
                    typed_array_sort_order(left, right)
                }
            });
            typed_array_from_values(kind, snapshot)
        }
        "with" => {
            let number = argument(0).to_number();
            if !number.is_finite() {
                binary_error("RangeError", "TypedArray index is outside the array");
            }
            let signed = number.trunc() as i64;
            let index = if signed < 0 {
                length as i64 + signed
            } else {
                signed
            };
            if index < 0 || index >= length as i64 {
                binary_error("RangeError", "TypedArray index is outside the array");
            }
            let mut snapshot = values();
            snapshot[index as usize] = argument(1);
            typed_array_from_values(kind, snapshot)
        }
        _ => return None,
    })
}

pub fn typed_array_property(value: &Value, key: &str) -> Option<Value> {
    let (bytes, offset, length, kind, buffer) = view_for(value)?;
    match key {
        "buffer" => Some(buffer),
        "byteOffset" => Some(Value::Number(offset as f64)),
        "byteLength" => Some(Value::Number((length * kind.bytes()) as f64)),
        "BYTES_PER_ELEMENT" => Some(Value::Number(kind.bytes() as f64)),
        "length" => Some(Value::Number(length as f64)),
        "set" | "subarray" | "slice" | "fill" | "copyWithin" | "reverse" | "sort" | "map"
        | "filter" | "keys" | "values" | "entries" | "toReversed" | "toSorted" | "with" => {
            let target = value.clone();
            let name = key.to_string();
            Some(Value::function(move |_, args| {
                typed_array_call_method(&target, &name, args).unwrap_or(Value::Undefined)
            }))
        }
        _ => {
            let index = key.parse::<usize>().ok()?;
            if index >= length {
                return Some(Value::Undefined);
            }
            let start = offset + index * kind.bytes();
            if start + kind.bytes() > bytes.borrow().len() {
                return Some(Value::Undefined);
            }
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
    if start + kind.bytes() > bytes.borrow().len() {
        return true;
    }
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
    bytes.borrow_mut()[offset..offset + encoded.len()].copy_from_slice(&encoded);
    sync_views(bytes);
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
                binary_error("TypeError", "DataView requires an ArrayBuffer");
            };
            let offset = to_index(args.get(1), 0, "DataView byteOffset is outside the buffer");
            let buffer_length = bytes.borrow().len();
            if offset > buffer_length {
                binary_error("RangeError", "DataView byteOffset is outside the buffer");
            }
            let remaining = buffer_length - offset;
            if is_resizable_buffer(&buffer) && args.get(2).is_none_or(Value::is_undefined) {
                warn_resizable_view_fallback();
            }
            let length = if args.get(2).is_some_and(|value| !value.is_undefined()) {
                let length = to_index(args.get(2), 0, "DataView byteLength is outside the buffer");
                if length > remaining {
                    binary_error("RangeError", "DataView byteLength is outside the buffer");
                }
                length
            } else {
                remaining
            };
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
                ("getFloat16", Kind::Float16),
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
                            to_index(args.first(), 0, "DataView offset is outside the view");
                        if index
                            .checked_add(kind.bytes())
                            .is_none_or(|end| end > length)
                            || offset + index + kind.bytes() > bytes.borrow().len()
                        {
                            binary_error("RangeError", "DataView offset is outside the view");
                        }
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
                ("setFloat16", Kind::Float16),
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
                            to_index(args.first(), 0, "DataView offset is outside the view");
                        if index
                            .checked_add(kind.bytes())
                            .is_none_or(|end| end > length)
                            || offset + index + kind.bytes() > bytes.borrow().len()
                        {
                            binary_error("RangeError", "DataView offset is outside the view");
                        }
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
    fn fill_array_buffer_view_writes_into_the_supplied_typed_array() {
        let dest =
            crate::class::construct(&typed_array_class("Uint8Array"), vec![Value::Number(4.0)]);
        dest.set_property("3", Value::Number(99.0));
        let filled = fill_array_buffer_view(&dest, &[7, 8, 9]).expect("fill typed array");
        assert!(is_array_buffer_view(&dest));
        assert!(
            filled
                .get_property("buffer")
                .strict_eq(&dest.get_property("buffer"))
        );
        assert_eq!(filled.get_property("byteLength").to_number(), 3.0);
        assert_eq!(dest.get_property("0").to_number(), 7.0);
        assert_eq!(dest.get_property("1").to_number(), 8.0);
        assert_eq!(dest.get_property("2").to_number(), 9.0);
        assert_eq!(dest.get_property("3").to_number(), 99.0);
        assert_eq!(filled.get_property("0").to_number(), 7.0);
        assert_eq!(bytes_of(&filled).unwrap(), vec![7, 8, 9]);
    }

    #[test]
    fn typed_array_methods_preserve_kind_ranges_and_backing_storage() {
        let words = crate::class::construct(
            &typed_array_class("Uint16Array"),
            vec![Value::array(vec![
                Value::Number(3.0),
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(4.0),
            ])],
        );
        assert!(words.get_property("subarray").is_function());

        let subarray = words.call_method("subarray", vec![Value::Number(1.0), Value::Number(3.0)]);
        assert!(typed_array_instance_of(
            &subarray,
            &typed_array_class("Uint16Array")
        ));
        assert_eq!(
            subarray.get_property("buffer"),
            words.get_property("buffer")
        );
        assert_eq!(subarray.get_property("byteOffset"), Value::Number(2.0));
        subarray.set_property("0", Value::Number(9.0));
        assert_eq!(words.get_property("1"), Value::Number(9.0));

        let slice = words.call_method("slice", vec![Value::Number(1.0), Value::Number(3.0)]);
        assert!(typed_array_instance_of(
            &slice,
            &typed_array_class("Uint16Array")
        ));
        assert_ne!(slice.get_property("buffer"), words.get_property("buffer"));
        slice.set_property("0", Value::Number(8.0));
        assert_eq!(words.get_property("1"), Value::Number(9.0));

        words.call_method(
            "fill",
            vec![Value::Number(7.0), Value::Number(1.0), Value::Number(3.0)],
        );
        words.call_method(
            "copyWithin",
            vec![Value::Number(0.0), Value::Number(2.0), Value::Number(4.0)],
        );
        assert_eq!(
            typed_array_values(&words).unwrap(),
            vec![
                Value::Number(7.0),
                Value::Number(4.0),
                Value::Number(7.0),
                Value::Number(4.0)
            ]
        );

        words.call_method("reverse", vec![]);
        words.call_method("sort", vec![]);
        assert_eq!(
            typed_array_values(&words).unwrap(),
            vec![
                Value::Number(4.0),
                Value::Number(4.0),
                Value::Number(7.0),
                Value::Number(7.0)
            ]
        );

        let mapped = words.call_method(
            "map",
            vec![Value::function(|_, args| {
                Value::Number(args[0].to_number() + 1.0)
            })],
        );
        let filtered = mapped.call_method(
            "filter",
            vec![Value::function(|_, args| {
                Value::Bool(args[0].to_number() > 5.0)
            })],
        );
        assert!(typed_array_instance_of(
            &mapped,
            &typed_array_class("Uint16Array")
        ));
        assert!(typed_array_instance_of(
            &filtered,
            &typed_array_class("Uint16Array")
        ));
        assert_eq!(filtered.get_property("length"), Value::Number(2.0));

        let reversed = words.call_method("toReversed", vec![]);
        assert_eq!(reversed.get_property("0"), Value::Number(7.0));
        assert_eq!(words.get_property("0"), Value::Number(4.0));
        let replaced = words.call_method("with", vec![Value::Number(-1.0), Value::Number(5.0)]);
        assert_eq!(replaced.get_property("3"), Value::Number(5.0));
        assert_eq!(words.get_property("3"), Value::Number(7.0));

        let iterator = words.call_method("entries", vec![]);
        let first = iterator.call_method("next", vec![]);
        assert_eq!(
            first.get_property("value").get_property("0"),
            Value::from(0)
        );
        assert_eq!(
            first.get_property("value").get_property("1"),
            Value::Number(4.0)
        );

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            words.call_method(
                "set",
                vec![
                    Value::array(vec![Value::Number(1.0), Value::Number(2.0)]),
                    Value::Number(3.0),
                ],
            )
        }));
        let error = outcome
            .expect_err("out-of-bounds TypedArray#set must fail")
            .downcast::<PanicValue>()
            .expect("exception should contain a JavaScript value");
        assert_eq!(error.0.get_property("name").to_js_string(), "RangeError");
    }

    #[test]
    fn typed_array_explicit_iterators_are_live_and_self_iterable() {
        let typed = typed_array_value(vec![Value::Number(1.0), Value::Number(2.0)]);
        let values = typed.call_method("values", vec![]);
        assert_eq!(
            values.call_method("next", vec![]).get_property("value"),
            Value::Number(1.0)
        );
        typed.set_property("1", Value::Number(9.0));
        assert_eq!(
            values.iter().next(),
            Some(Value::Number(9.0)),
            "iterating an iterator must continue the same live cursor"
        );

        let entries = typed.call_method("entries", vec![]);
        typed.set_property("0", Value::Number(7.0));
        let first = entries.call_method("next", vec![]).get_property("value");
        assert_eq!(first.get_property("0"), Value::from(0));
        assert_eq!(first.get_property("1"), Value::Number(7.0));

        assert_eq!(
            typed.call_method("keys", vec![]).iter().collect::<Vec<_>>(),
            vec![Value::from(0), Value::from(1)]
        );
    }

    #[test]
    fn binary_constructors_and_data_view_enforce_web_bounds_errors() {
        let fractional = crate::class::construct(&array_buffer_class(), vec![Value::Number(3.9)]);
        assert_eq!(fractional.get_property("byteLength"), Value::Number(3.0));

        let error_name = |operation: Box<dyn FnOnce()>| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
                .expect_err("operation must throw")
                .downcast::<PanicValue>()
                .expect("exception should contain a JavaScript value")
                .0
                .get_property("name")
                .to_js_string()
        };
        assert_eq!(
            error_name(Box::new(|| {
                crate::class::construct(&array_buffer_class(), vec![Value::Number(-1.0)]);
            })),
            "RangeError"
        );

        let buffer = crate::class::construct(&array_buffer_class(), vec![Value::Number(8.0)]);
        let misaligned = buffer.clone();
        assert_eq!(
            error_name(Box::new(move || {
                crate::class::construct(
                    &typed_array_class("Uint16Array"),
                    vec![misaligned, Value::Number(1.0)],
                );
            })),
            "RangeError"
        );
        let overflowing = buffer.clone();
        assert_eq!(
            error_name(Box::new(move || {
                crate::class::construct(
                    &typed_array_class("Uint16Array"),
                    vec![overflowing, Value::Number(6.0), Value::Number(2.0)],
                );
            })),
            "RangeError"
        );
        let invalid_view = buffer.clone();
        assert_eq!(
            error_name(Box::new(move || {
                crate::class::construct(
                    &data_view_class(),
                    vec![invalid_view, Value::Number(7.0), Value::Number(2.0)],
                );
            })),
            "RangeError"
        );
        assert_eq!(
            error_name(Box::new(|| {
                crate::class::construct(&data_view_class(), vec![Value::object(HashMap::new())]);
            })),
            "TypeError"
        );

        let view = crate::class::construct(
            &data_view_class(),
            vec![buffer, Value::Number(2.0), Value::Number(2.0)],
        );
        let read_view = view.clone();
        assert_eq!(
            error_name(Box::new(move || {
                read_view.call_method("getUint16", vec![Value::Number(1.0)]);
            })),
            "RangeError"
        );
        assert_eq!(
            error_name(Box::new(move || {
                view.call_method("setUint32", vec![Value::Number(0.0), Value::Number(1.0)]);
            })),
            "RangeError"
        );
    }

    #[test]
    fn resizable_and_growable_buffers_enforce_capacity_and_transfer_semantics() {
        let options = Value::object(HashMap::from([(
            "maxByteLength".into(),
            Value::Number(16.0),
        )]));
        let buffer = crate::class::construct(
            &array_buffer_class(),
            vec![Value::Number(4.0), options.clone()],
        );
        assert_eq!(buffer.get_property("byteLength"), Value::Number(4.0));
        assert_eq!(buffer.get_property("maxByteLength"), Value::Number(16.0));
        assert_eq!(buffer.get_property("resizable"), Value::Bool(true));
        assert_eq!(buffer.get_property("detached"), Value::Bool(false));

        let bytes = crate::class::construct(&typed_array_class("Uint8Array"), vec![buffer.clone()]);
        bytes.set_property("0", Value::Number(42.0));
        buffer.call_method("resize", vec![Value::Number(8.0)]);
        assert_eq!(buffer.get_property("byteLength"), Value::Number(8.0));
        assert_eq!(bytes.get_property("0"), Value::Number(42.0));

        let transferred = buffer.call_method("transfer", vec![Value::Number(6.0)]);
        assert_eq!(buffer.get_property("byteLength"), Value::Number(0.0));
        assert_eq!(buffer.get_property("maxByteLength"), Value::Number(0.0));
        assert_eq!(buffer.get_property("resizable"), Value::Bool(false));
        assert_eq!(buffer.get_property("detached"), Value::Bool(true));
        assert_eq!(transferred.get_property("byteLength"), Value::Number(6.0));
        assert_eq!(
            transferred.get_property("maxByteLength"),
            Value::Number(16.0)
        );
        assert_eq!(transferred.get_property("resizable"), Value::Bool(true));
        assert_eq!(bytes_of(&transferred).unwrap()[0], 42);

        let fixed = transferred.call_method("transferToFixedLength", vec![]);
        assert_eq!(fixed.get_property("byteLength"), Value::Number(6.0));
        assert_eq!(fixed.get_property("maxByteLength"), Value::Number(6.0));
        assert_eq!(fixed.get_property("resizable"), Value::Bool(false));

        let shared = crate::class::construct(
            &shared_array_buffer_class(),
            vec![Value::Number(4.0), options],
        );
        assert_eq!(shared.get_property("growable"), Value::Bool(true));
        shared.call_method("grow", vec![Value::Number(12.0)]);
        assert_eq!(shared.get_property("byteLength"), Value::Number(12.0));
        assert_eq!(shared.get_property("maxByteLength"), Value::Number(16.0));

        let error_name = |operation: Box<dyn FnOnce()>| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
                .expect_err("operation must throw")
                .downcast::<PanicValue>()
                .expect("exception should contain a JavaScript value")
                .0
                .get_property("name")
                .to_js_string()
        };
        let fixed_resize = fixed.clone();
        assert_eq!(
            error_name(Box::new(move || {
                fixed_resize.call_method("resize", vec![Value::Number(7.0)]);
            })),
            "TypeError"
        );
        let shared_shrink = shared.clone();
        assert_eq!(
            error_name(Box::new(move || {
                shared_shrink.call_method("grow", vec![Value::Number(8.0)]);
            })),
            "RangeError"
        );
        assert_eq!(
            error_name(Box::new(move || {
                shared.call_method("grow", vec![Value::Number(17.0)]);
            })),
            "RangeError"
        );
    }

    #[test]
    fn float16_array_and_data_view_round_ieee_binary16_values() {
        let halves = crate::class::construct(
            &typed_array_class("Float16Array"),
            vec![Value::array(vec![
                Value::Number(1.5),
                Value::Number(1.000_488_281_25),
                Value::Number(f64::INFINITY),
                Value::Number(f64::NAN),
            ])],
        );
        assert!(typed_array_instance_of(
            &halves,
            &typed_array_class("Float16Array")
        ));
        assert_eq!(halves.get_property("BYTES_PER_ELEMENT"), Value::Number(2.0));
        assert_eq!(halves.get_property("0"), Value::Number(1.5));
        assert_eq!(
            halves.get_property("1"),
            Value::Number(1.0),
            "halfway values round to nearest-even binary16"
        );
        assert!(halves.get_property("2").to_number().is_infinite());
        assert!(halves.get_property("3").to_number().is_nan());

        let view = crate::class::construct(&data_view_class(), vec![halves.get_property("buffer")]);
        assert_eq!(
            view.call_method("getFloat16", vec![Value::Number(0.0), Value::Bool(true)]),
            Value::Number(1.5)
        );
        view.call_method(
            "setFloat16",
            vec![Value::Number(2.0), Value::Number(-2.0), Value::Bool(true)],
        );
        assert_eq!(halves.get_property("1"), Value::Number(-2.0));
        assert_eq!(
            crate::builtins::Math.call_method("f16round", vec![Value::Number(1.000_488_281_25)]),
            Value::Number(1.0)
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
