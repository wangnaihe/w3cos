//! Web platform builtins for the ESM compile pipeline: `Intl`, `Date`,
//! `atob` / `btoa`, `structuredClone`, `URL`, `URLSearchParams`, and
//! `URLPattern`.
//!
//! Everything is hand-rolled on top of `base64` (percent-encoding and the
//! URL grammar are small enough to keep local — no `url` crate dependency).
//! Errors are raised as JS exceptions via `std::panic::panic_any(error)`
//! where the web platform would throw (`InvalidCharacterError`,
//! `DataCloneError`, `TypeError: Invalid URL`).
//!
//! A `URL` value stores its components in a shared `UrlParts` behind the
//! runtime's `__w3cos_getter_*` / `__w3cos_setter_*` property conventions,
//! so reads and writes of `protocol`/`host`/`pathname`/... stay consistent
//! with `href`/`toString`. Its `searchParams` object shares the parts back
//! (mutating params updates `search`), but writing `search` directly does
//! NOT rebuild an already-exposed `searchParams` object (v1 limitation).
//! `structuredClone` preserves cycles/shared references and the runtime's
//! BigInt, Date, RegExp, Map, Set, Error, DOMException, Blob, File, ImageData,
//! ArrayBuffer, SharedArrayBuffer, TypedArray, and DataView representations.
//! Functions raise `DataCloneError`; ArrayBuffer transfer/detach and
//! two-phase host-registered transferable objects are supported.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{Offset, TimeZone};

use crate::Value;
use crate::value::js_error;

thread_local! {
    static DOM_EXCEPTION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IMAGE_DATA_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static NEXT_BLOB_ID: Cell<u64> = const { Cell::new(1) };
    static BLOBS: RefCell<HashMap<u64, Rc<BlobState>>> = RefCell::new(HashMap::new());
    static BLOB_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FILE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TEXT_DECODER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static HOST_TRANSFERABLES: RefCell<HashMap<usize, (Value, Value)>> =
        RefCell::new(HashMap::new());
}

const BLOB_STATE_KEY: &str = "__w3cos_blob_id";

#[derive(Clone)]
struct BlobState {
    bytes: Vec<u8>,
    type_name: String,
}

const DOM_EXCEPTION_CODES: &[(&str, &str, f64)] = &[
    ("INDEX_SIZE_ERR", "IndexSizeError", 1.0),
    ("DOMSTRING_SIZE_ERR", "DOMStringSizeError", 2.0),
    ("HIERARCHY_REQUEST_ERR", "HierarchyRequestError", 3.0),
    ("WRONG_DOCUMENT_ERR", "WrongDocumentError", 4.0),
    ("INVALID_CHARACTER_ERR", "InvalidCharacterError", 5.0),
    ("NO_DATA_ALLOWED_ERR", "NoDataAllowedError", 6.0),
    (
        "NO_MODIFICATION_ALLOWED_ERR",
        "NoModificationAllowedError",
        7.0,
    ),
    ("NOT_FOUND_ERR", "NotFoundError", 8.0),
    ("NOT_SUPPORTED_ERR", "NotSupportedError", 9.0),
    ("INUSE_ATTRIBUTE_ERR", "InUseAttributeError", 10.0),
    ("INVALID_STATE_ERR", "InvalidStateError", 11.0),
    ("SYNTAX_ERR", "SyntaxError", 12.0),
    ("INVALID_MODIFICATION_ERR", "InvalidModificationError", 13.0),
    ("NAMESPACE_ERR", "NamespaceError", 14.0),
    ("INVALID_ACCESS_ERR", "InvalidAccessError", 15.0),
    ("VALIDATION_ERR", "ValidationError", 16.0),
    ("TYPE_MISMATCH_ERR", "TypeMismatchError", 17.0),
    ("SECURITY_ERR", "SecurityError", 18.0),
    ("NETWORK_ERR", "NetworkError", 19.0),
    ("ABORT_ERR", "AbortError", 20.0),
    ("URL_MISMATCH_ERR", "URLMismatchError", 21.0),
    ("QUOTA_EXCEEDED_ERR", "QuotaExceededError", 22.0),
    ("TIMEOUT_ERR", "TimeoutError", 23.0),
    ("INVALID_NODE_TYPE_ERR", "InvalidNodeTypeError", 24.0),
    ("DATA_CLONE_ERR", "DataCloneError", 25.0),
];

fn dom_exception_code(name: &str) -> f64 {
    DOM_EXCEPTION_CODES
        .iter()
        .find_map(|(_, exception_name, code)| (*exception_name == name).then_some(*code))
        .unwrap_or(0.0)
}

fn initialize_dom_exception(this: &Value, args: &[Value]) {
    let message = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::string(""))
        .to_js_string();
    let name = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| Value::string("Error"))
        .to_js_string();
    this.set_property("message", Value::string(&message));
    this.set_property("name", Value::string(&name));
    this.set_property("code", Value::Number(dom_exception_code(&name)));
    this.set_property(
        "stack",
        Value::from(if message.is_empty() {
            name
        } else {
            format!("{name}: {message}")
        }),
    );
}

pub fn dom_exception_class() -> Value {
    DOM_EXCEPTION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            initialize_dom_exception(&this, &args);
            Value::Undefined
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["code", "message", "name"] {
            prototype.set_property(property, Value::Undefined);
        }
        prototype.set_property(
            "toString",
            Value::function(|this, _| {
                let name = this.get_property("name").to_js_string();
                let message = this.get_property("message").to_js_string();
                Value::from(if message.is_empty() {
                    name
                } else {
                    format!("{name}: {message}")
                })
            }),
        );
        for (constant, _, code) in DOM_EXCEPTION_CODES {
            class.set_property(constant, Value::Number(*code));
            prototype.set_property(constant, Value::Number(*code));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn dom_exception_instance(message: &str, name: &str) -> Value {
    crate::class::construct(
        &dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

pub fn image_data_class() -> Value {
    IMAGE_DATA_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let first = args.first().cloned().unwrap_or_default();
            let (data, width, height, settings_index) = if crate::binary::is_typed_array(&first) {
                let width = args.get(1).map(Value::to_u32).unwrap_or(0);
                let bytes = crate::binary::bytes_of(&first).unwrap_or_default();
                let explicit_height = args.get(2).filter(|value| value.is_number());
                let height = explicit_height
                    .map(Value::to_u32)
                    .unwrap_or_else(|| (bytes.len() as u32 / 4) / width.max(1));
                (
                    first,
                    width,
                    height,
                    usize::from(explicit_height.is_some()) + 2,
                )
            } else {
                let width = first.to_u32();
                let height = args.get(1).map(Value::to_u32).unwrap_or(0);
                let data = crate::class::construct(
                    &crate::binary::typed_array_class("Uint8ClampedArray"),
                    vec![Value::Number((width * height * 4) as f64)],
                );
                (data, width, height, 2)
            };
            let color_space = args
                .get(settings_index)
                .map(|settings| settings.get_property("colorSpace").to_js_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "srgb".to_string());
            image_data_value(data, width, height, &color_space)
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["colorSpace", "data", "height", "pixelFormat", "width"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn image_data_value(data: Value, width: u32, height: u32, color_space: &str) -> Value {
    let value = Value::object(HashMap::from([
        ("data".to_string(), data),
        ("width".to_string(), Value::Number(width as f64)),
        ("height".to_string(), Value::Number(height as f64)),
        ("colorSpace".to_string(), Value::string(color_space)),
    ]));
    crate::class::set_prototype_of(&value, &image_data_class().get_property("prototype"));
    value
}

fn blob_state(value: &Value) -> Option<Rc<BlobState>> {
    let Some(object) = value.as_object() else {
        return None;
    };
    let Some(id) = object.borrow().get_direct(BLOB_STATE_KEY).as_number() else {
        return None;
    };
    BLOBS.with(|states| states.borrow().get(&(id as u64)).cloned())
}

pub fn blob_bytes(value: &Value) -> Option<Vec<u8>> {
    blob_state(value).map(|state| state.bytes.clone())
}

fn blob_part_bytes(value: &Value) -> Vec<u8> {
    if let Some(bytes) = blob_bytes(value) {
        return bytes;
    }
    if let Some(bytes) = crate::binary::bytes_of(value) {
        return bytes;
    }
    value.to_js_string().into_bytes()
}

fn normalize_blob_index(value: Option<&Value>, length: usize, fallback: usize) -> usize {
    let number = value.map(Value::to_number).unwrap_or(fallback as f64);
    if number.is_sign_negative() {
        (length as i64 + number as i64).max(0) as usize
    } else {
        (number.max(0.0) as usize).min(length)
    }
}

fn make_blob(bytes: Vec<u8>, type_name: String, prototype: Value) -> Value {
    let id = NEXT_BLOB_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let state = Rc::new(BlobState {
        bytes,
        type_name: type_name.to_ascii_lowercase(),
    });
    BLOBS.with(|states| states.borrow_mut().insert(id, state.clone()));
    let value = Value::object(HashMap::from([(
        BLOB_STATE_KEY.to_string(),
        Value::Number(id as f64),
    )]));
    crate::class::set_prototype_of(&value, &prototype);
    value.set_property("size", Value::Number(state.bytes.len() as f64));
    value.set_property("type", Value::string(&state.type_name));

    let state_for_text = state.clone();
    value.set_property(
        "text",
        Value::function(move |_, _| Value::string(&String::from_utf8_lossy(&state_for_text.bytes))),
    );
    let state_for_buffer = state.clone();
    value.set_property(
        "arrayBuffer",
        Value::function(move |_, _| {
            crate::binary::array_buffer_value(state_for_buffer.bytes.clone())
        }),
    );
    let state_for_bytes = state.clone();
    value.set_property(
        "bytes",
        Value::function(move |_, _| {
            crate::binary::typed_array_value(
                state_for_bytes
                    .bytes
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            )
        }),
    );
    let state_for_slice = state;
    value.set_property(
        "slice",
        Value::function(move |_, args| {
            let length = state_for_slice.bytes.len();
            let start = normalize_blob_index(args.first(), length, 0);
            let end = normalize_blob_index(args.get(1), length, length).max(start);
            let type_name = args.get(2).map(Value::to_js_string).unwrap_or_default();
            make_blob(
                state_for_slice.bytes[start..end].to_vec(),
                type_name,
                blob_class().get_property("prototype"),
            )
        }),
    );
    value
}

fn construct_blob(args: &[Value], prototype: Value) -> Value {
    let parts = args.first().cloned().unwrap_or_default();
    let options = args.get(1).cloned().unwrap_or_default();
    let bytes = parts
        .iter()
        .flat_map(|part| blob_part_bytes(&part))
        .collect::<Vec<_>>();
    let type_name = if options.is_object() {
        options.get_property("type").to_js_string()
    } else {
        String::new()
    };
    make_blob(bytes, type_name, prototype)
}

pub fn blob_class() -> Value {
    BLOB_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            construct_blob(&args, blob_class().get_property("prototype"))
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["arrayBuffer", "bytes", "slice", "stream", "text"] {
            prototype.set_property(
                method,
                if method == "stream" {
                    Value::function(|_, _| {
                        eprintln!(
                            "[w3cos] warning: Blob.stream requires the runtime Streams adapter; \
                             use arrayBuffer(), bytes(), or text() in core-only execution"
                        );
                        Value::Undefined
                    })
                } else {
                    Value::function(|_, _| Value::Undefined)
                },
            );
        }
        for property in ["size", "type"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn file_value(bytes: Vec<u8>, type_name: String, name: String, last_modified: f64) -> Value {
    let value = make_blob(bytes, type_name, file_class().get_property("prototype"));
    value.set_property("name", Value::string(&name.replace('/', ":")));
    value.set_property("webkitRelativePath", Value::string(""));
    value.set_property("lastModified", Value::Number(last_modified));
    value
}

pub fn file_class() -> Value {
    FILE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let parts = args.first().cloned().unwrap_or_default();
            let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
            let options = args.get(2).cloned().unwrap_or_default();
            let bytes = parts
                .iter()
                .flat_map(|part| blob_part_bytes(&part))
                .collect::<Vec<_>>();
            let type_name = options.get_property("type").to_js_string();
            let last_modified = options.get_property("lastModified");
            file_value(
                bytes,
                type_name,
                name,
                if last_modified.is_undefined() {
                    now_milliseconds()
                } else {
                    last_modified.to_number()
                },
            )
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "lastModified",
            "lastModifiedDate",
            "name",
            "webkitRelativePath",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        crate::class::set_prototype_of(&prototype, &blob_class().get_property("prototype"));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

// ── atob / btoa ────────────────────────────────────────────────────────

pub fn text_decoder_class() -> Value {
    TEXT_DECODER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let label = args.first().cloned().unwrap_or(Value::Undefined);
            let encoding = if label.is_undefined() {
                "utf-8".to_string()
            } else {
                match label
                    .to_js_string()
                    .trim()
                    .to_ascii_lowercase()
                    .replace('_', "-")
                    .as_str()
                {
                    "unicode-1-1-utf-8" | "utf8" | "utf-8" => "utf-8".to_string(),
                    "utf-16" | "utf-16le" | "utf16le" => "utf-16le".to_string(),
                    "utf-16be" | "utf16be" => "utf-16be".to_string(),
                    other => {
                        eprintln!(
                            "[w3cos] warning: TextDecoder encoding {other:?} uses the UTF-8 \
                             compatibility fallback"
                        );
                        "utf-8".to_string()
                    }
                }
            };
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let fatal = options.get_property("fatal").to_bool();
            let ignore_bom = options.get_property("ignoreBOM").to_bool();
            let decode_encoding = encoding.clone();
            let state = Rc::new(RefCell::new(DecoderStreamState::default()));
            let value = Value::object(HashMap::from([
                ("encoding".to_string(), Value::string(&encoding)),
                ("fatal".to_string(), Value::Bool(fatal)),
                ("ignoreBOM".to_string(), Value::Bool(ignore_bom)),
                (
                    "decode".to_string(),
                    Value::function(move |_this, args| {
                        let input = args.first().cloned().unwrap_or(Value::Undefined);
                        let decode_options = args.get(1).cloned().unwrap_or(Value::Undefined);
                        let stream = decode_options.get_property("stream").to_bool();
                        if decode_encoding.starts_with("utf-16")
                            && !stream
                            && !crate::binary::is_typed_array(&input)
                        {
                            let units: Vec<u16> =
                                input.iter().map(|value| value.to_number() as u16).collect();
                            return match String::from_utf16(&units) {
                                Ok(text) => Value::from(text),
                                Err(_) if fatal => {
                                    crate::throw_value(Value::object(HashMap::from([
                                        ("name".into(), Value::string("TypeError")),
                                        (
                                            "message".into(),
                                            Value::string("The encoded data was not valid UTF-16"),
                                        ),
                                    ])))
                                }
                                Err(_) => Value::from(String::from_utf16_lossy(&units)),
                            };
                        }
                        let bytes = crate::binary::bytes_of(&input).unwrap_or_else(|| {
                            input.iter().map(|value| value.to_number() as u8).collect()
                        });
                        match decode_incremental(
                            &decode_encoding,
                            fatal,
                            ignore_bom,
                            &mut state.borrow_mut(),
                            bytes,
                            stream,
                        ) {
                            Ok(text) => Value::from(text),
                            Err(message) => crate::throw_value(Value::object(HashMap::from([
                                ("name".into(), Value::string("TypeError")),
                                ("message".into(), Value::string(&message)),
                            ]))),
                        }
                    }),
                ),
            ]));
            crate::class::set_prototype_of(&value, &text_decoder_class().get_property("prototype"));
            value
        });
        class.set_property("name", Value::string("TextDecoder"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("decode", Value::function(|_, _| Value::Undefined));
        for property in ["encoding", "fatal", "ignoreBOM"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

struct DecoderStreamState {
    pending: Vec<u8>,
    at_start: bool,
}

impl Default for DecoderStreamState {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            at_start: true,
        }
    }
}

fn decode_incremental(
    encoding: &str,
    fatal: bool,
    ignore_bom: bool,
    state: &mut DecoderStreamState,
    bytes: Vec<u8>,
    stream: bool,
) -> Result<String, String> {
    state.pending.extend(bytes);
    let mut input = std::mem::take(&mut state.pending);

    if state.at_start {
        let bom: &[u8] = match encoding {
            "utf-8" => &[0xef, 0xbb, 0xbf],
            "utf-16le" => &[0xff, 0xfe],
            "utf-16be" => &[0xfe, 0xff],
            _ => &[],
        };
        if !ignore_bom && stream && input.len() < bom.len() && bom.starts_with(&input) {
            state.pending = input;
            return Ok(String::new());
        }
        if !ignore_bom && !bom.is_empty() && input.starts_with(bom) {
            input.drain(..bom.len());
        }
        state.at_start = false;
    }

    let result = if encoding == "utf-8" {
        if stream {
            let incomplete = utf8_incomplete_suffix_len(&input);
            if incomplete > 0 {
                state.pending = input.split_off(input.len() - incomplete);
            }
        }
        match String::from_utf8(input) {
            Ok(text) => Ok(text),
            Err(error) if fatal => Err(format!(
                "The encoded data was not valid UTF-8 at byte {}",
                error.utf8_error().valid_up_to()
            )),
            Err(error) => Ok(String::from_utf8_lossy(error.as_bytes()).into_owned()),
        }
    } else {
        if stream && input.len() % 2 != 0 {
            state.pending.insert(0, input.pop().unwrap_or_default());
        }
        let mut units = input
            .chunks(2)
            .map(|chunk| {
                if chunk.len() < 2 {
                    0xfffd
                } else if encoding == "utf-16be" {
                    u16::from_be_bytes([chunk[0], chunk[1]])
                } else {
                    u16::from_le_bytes([chunk[0], chunk[1]])
                }
            })
            .collect::<Vec<_>>();
        if stream
            && units
                .last()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        {
            units.pop();
            let split = input.len() - 2;
            state.pending.splice(0..0, input[split..].iter().copied());
        }
        match String::from_utf16(&units) {
            Ok(text) => Ok(text),
            Err(_) if fatal => Err("The encoded data was not valid UTF-16".into()),
            Err(_) => Ok(String::from_utf16_lossy(&units)),
        }
    };

    if !stream || result.is_err() {
        state.pending.clear();
        state.at_start = true;
    }
    result
}

fn utf8_incomplete_suffix_len(bytes: &[u8]) -> usize {
    let Some(mut index) = bytes.len().checked_sub(1) else {
        return 0;
    };
    while index > 0 && bytes[index] & 0xc0 == 0x80 {
        index -= 1;
    }
    let first = bytes[index];
    let expected = match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return 0,
    };
    let available = bytes.len() - index;
    (available < expected).then_some(available).unwrap_or(0)
}

/// Compact `Date` constructor used by the native JavaScript runtime. Date
/// instances keep their epoch milliseconds in a non-standard internal slot;
/// Web APIs such as IndexedDB recognize the slot while applications interact
/// through standard Date methods.
pub fn date_class() -> Value {
    Value::callable(
        HashMap::from([
            (
                "now".into(),
                Value::function(|_, _| Value::Number(now_milliseconds())),
            ),
            (
                "parse".into(),
                Value::function(|_, args| {
                    Value::Number(
                        args.first()
                            .map(|value| parse_iso_instant(&value.to_js_string()))
                            .unwrap_or(f64::NAN),
                    )
                }),
            ),
        ]),
        |_this, args| {
            let milliseconds = args
                .first()
                .map(|value| match value {
                    value if value.is_string() => parse_iso_instant(&value.to_js_string()),
                    _ => value.to_number(),
                })
                .unwrap_or_else(now_milliseconds);
            date_value(milliseconds)
        },
    )
}

pub fn date_value(milliseconds: f64) -> Value {
    Value::object(HashMap::from([
        (
            "__w3cos_date_milliseconds".into(),
            Value::Number(milliseconds),
        ),
        (
            "getTime".into(),
            Value::function(move |_, _| Value::Number(milliseconds)),
        ),
        (
            "valueOf".into(),
            Value::function(move |_, _| Value::Number(milliseconds)),
        ),
        (
            "toISOString".into(),
            Value::function(move |_, _| {
                let text = chrono::Utc
                    .timestamp_millis_opt(milliseconds.floor() as i64)
                    .single()
                    .map(|instant| instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                    .unwrap_or_else(|| "Invalid Date".to_string());
                Value::from(text)
            }),
        ),
    ]))
}

fn now_milliseconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(f64::NAN)
}

/// Compact `Intl` implementation for the native ESM runtime.
///
/// The first compatibility tier intentionally covers selected locale-sensitive
/// formatting used by native applications. Unsupported locales fall back to
/// `en-US`; unsupported timezones fail deterministically instead of silently
/// formatting in the host timezone.
pub fn intl_value() -> Value {
    Value::object(HashMap::from([
        ("NumberFormat".into(), number_format_class()),
        ("DateTimeFormat".into(), date_time_format_class()),
    ]))
}

fn number_format_class() -> Value {
    Value::callable(HashMap::new(), |_this, args| {
        let locale = canonical_locale(args.first());
        let options = args.get(1).cloned().unwrap_or(Value::Undefined);
        let style = string_option(&options, "style").unwrap_or_else(|| "decimal".into());
        let currency = string_option(&options, "currency");
        let currency_display =
            string_option(&options, "currencyDisplay").unwrap_or_else(|| "symbol".into());
        let default_fraction_digits = if style == "currency" {
            currency
                .as_deref()
                .map(currency_fraction_digits)
                .unwrap_or(2)
        } else {
            3
        };
        let minimum_fraction_digits =
            usize_option(&options, "minimumFractionDigits").unwrap_or(if style == "currency" {
                default_fraction_digits
            } else {
                0
            });
        let maximum_fraction_digits = usize_option(&options, "maximumFractionDigits")
            .unwrap_or(default_fraction_digits)
            .max(minimum_fraction_digits)
            .min(20);
        let use_grouping = bool_option(&options, "useGrouping").unwrap_or(true);
        let locale_for_format = locale.clone();
        let style_for_format = style.clone();
        let currency_for_format = currency.clone();
        let currency_display_for_format = currency_display.clone();
        let format = Value::function(move |_, args| {
            let number = args.first().map(Value::to_number).unwrap_or(f64::NAN);
            Value::string(&format_number(
                number,
                &locale_for_format,
                &style_for_format,
                currency_for_format.as_deref(),
                &currency_display_for_format,
                minimum_fraction_digits,
                maximum_fraction_digits,
                use_grouping,
            ))
        });
        let locale_for_options = locale.clone();
        let currency_for_options = currency.clone();
        Value::object(HashMap::from([
            ("format".into(), format),
            (
                "resolvedOptions".into(),
                Value::function(move |_, _| {
                    let mut resolved = HashMap::from([
                        ("locale".into(), Value::string(&locale_for_options)),
                        ("style".into(), Value::string(&style)),
                        (
                            "minimumFractionDigits".into(),
                            Value::Number(minimum_fraction_digits as f64),
                        ),
                        (
                            "maximumFractionDigits".into(),
                            Value::Number(maximum_fraction_digits as f64),
                        ),
                        ("useGrouping".into(), Value::Bool(use_grouping)),
                    ]);
                    if let Some(currency) = &currency_for_options {
                        resolved.insert("currency".into(), Value::string(currency));
                    }
                    Value::object(resolved)
                }),
            ),
        ]))
    })
}

fn date_time_format_class() -> Value {
    Value::callable(HashMap::new(), |_this, args| {
        let locale = canonical_locale(args.first());
        let options = args.get(1).cloned().unwrap_or(Value::Undefined);
        let time_zone = string_option(&options, "timeZone").unwrap_or_else(|| "UTC".into());
        let time_zone_spec = parse_time_zone(&time_zone).unwrap_or_else(|| {
            crate::throw_value(js_error(&format!(
                "RangeError: unsupported time zone: {time_zone}"
            )))
        });
        let date_style = string_option(&options, "dateStyle");
        let time_style = string_option(&options, "timeStyle");
        let locale_for_format = locale.clone();
        let time_zone_for_options = time_zone.clone();
        let date_style_for_format = date_style.clone();
        let time_style_for_format = time_style.clone();
        let format = Value::function(move |_, args| {
            let milliseconds = args
                .first()
                .map(date_milliseconds)
                .unwrap_or_else(now_milliseconds);
            if !milliseconds.is_finite() {
                crate::throw_value(js_error("RangeError: invalid time value"));
            }
            Value::string(&format_date_time(
                milliseconds,
                time_zone_spec.offset_minutes(milliseconds),
                &locale_for_format,
                date_style_for_format.as_deref(),
                time_style_for_format.as_deref(),
            ))
        });
        let locale_for_options = locale.clone();
        Value::object(HashMap::from([
            ("format".into(), format),
            (
                "resolvedOptions".into(),
                Value::function(move |_, _| {
                    Value::object(HashMap::from([
                        ("locale".into(), Value::string(&locale_for_options)),
                        ("timeZone".into(), Value::string(&time_zone_for_options)),
                    ]))
                }),
            ),
        ]))
    })
}

fn canonical_locale(value: Option<&Value>) -> String {
    let locale = value
        .filter(|value| !value.is_nullish())
        .map(Value::to_js_string)
        .unwrap_or_else(|| "en-US".into());
    match locale
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "zh" => "zh-CN".into(),
        "de" => "de-DE".into(),
        "fr" => "fr-FR".into(),
        "ja" => "ja-JP".into(),
        _ => "en-US".into(),
    }
}

fn string_option(options: &Value, key: &str) -> Option<String> {
    let value = options.get_property(key);
    (!value.is_nullish()).then(|| value.to_js_string())
}

fn usize_option(options: &Value, key: &str) -> Option<usize> {
    let value = options.get_property(key);
    (!value.is_nullish()).then(|| value.to_number().max(0.0) as usize)
}

fn bool_option(options: &Value, key: &str) -> Option<bool> {
    let value = options.get_property(key);
    (!value.is_nullish()).then(|| value.to_bool())
}

#[allow(clippy::too_many_arguments)]
fn format_number(
    number: f64,
    locale: &str,
    style: &str,
    currency: Option<&str>,
    currency_display: &str,
    minimum_fraction_digits: usize,
    maximum_fraction_digits: usize,
    use_grouping: bool,
) -> String {
    if number.is_nan() {
        return "NaN".into();
    }
    if number.is_infinite() {
        return if number.is_sign_negative() {
            "-∞".into()
        } else {
            "∞".into()
        };
    }
    let negative = number.is_sign_negative();
    let mut decimal = format!("{:.*}", maximum_fraction_digits, number.abs());
    if let Some(dot) = decimal.find('.') {
        while decimal.len() > dot + 1 + minimum_fraction_digits && decimal.ends_with('0') {
            decimal.pop();
        }
        if decimal.ends_with('.') {
            decimal.pop();
        }
    }
    let (integer, fraction) = decimal
        .split_once('.')
        .map(|(integer, fraction)| (integer, Some(fraction)))
        .unwrap_or((&decimal, None));
    let profile = locale_profile(locale);
    let integer = if use_grouping {
        group_decimal(integer, profile.grouping_separator)
    } else {
        integer.to_string()
    };
    let formatted = if let Some(fraction) = fraction {
        format!("{integer}{}{fraction}", profile.decimal_separator)
    } else {
        integer
    };
    let formatted = if style == "currency" {
        let currency = currency.unwrap_or("XXX").to_ascii_uppercase();
        let display = match currency_display {
            "code" => currency.clone(),
            "name" => currency_name(&currency, locale).into(),
            _ => currency_symbol(&currency).unwrap_or(&currency).into(),
        };
        if profile.currency_suffix {
            format!("{formatted} {display}")
        } else if currency_display == "code" || currency_display == "name" {
            format!("{display} {formatted}")
        } else {
            format!("{display}{formatted}")
        }
    } else {
        formatted
    };
    if negative {
        format!("-{formatted}")
    } else {
        formatted
    }
}

fn group_decimal(integer: &str, separator: char) -> String {
    let mut grouped = String::with_capacity(integer.len() + integer.len() / 3);
    for (index, character) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(separator);
        }
        grouped.push(character);
    }
    grouped
}

#[derive(Clone, Copy)]
struct LocaleProfile {
    decimal_separator: char,
    grouping_separator: char,
    currency_suffix: bool,
}

fn locale_profile(locale: &str) -> LocaleProfile {
    match locale {
        "de-DE" => LocaleProfile {
            decimal_separator: ',',
            grouping_separator: '.',
            currency_suffix: true,
        },
        "fr-FR" => LocaleProfile {
            decimal_separator: ',',
            grouping_separator: '\u{202f}',
            currency_suffix: true,
        },
        _ => LocaleProfile {
            decimal_separator: '.',
            grouping_separator: ',',
            currency_suffix: false,
        },
    }
}

fn currency_fraction_digits(currency: &str) -> usize {
    match currency.to_ascii_uppercase().as_str() {
        "JPY" | "KRW" => 0,
        _ => 2,
    }
}

fn currency_symbol(currency: &str) -> Option<&'static str> {
    match currency {
        "CNY" | "JPY" => Some("¥"),
        "USD" => Some("$"),
        "EUR" => Some("€"),
        "GBP" => Some("£"),
        _ => None,
    }
}

fn currency_name(currency: &str, locale: &str) -> &'static str {
    match (currency, locale) {
        ("CNY", "zh-CN") => "人民币",
        ("USD", "zh-CN") => "美元",
        ("EUR", "zh-CN") => "欧元",
        ("CNY", "de-DE") => "Chinesische Yuan",
        ("USD", "de-DE") => "US-Dollar",
        ("EUR", "de-DE") => "Euro",
        ("CNY", "fr-FR") => "yuans renminbi chinois",
        ("USD", "fr-FR") => "dollars des États-Unis",
        ("EUR", "fr-FR") => "euros",
        ("CNY", "ja-JP") => "中国人民元",
        ("USD", "ja-JP") => "米ドル",
        ("EUR", "ja-JP") => "ユーロ",
        ("CNY", _) => "Chinese yuan",
        ("USD", _) => "US dollars",
        ("EUR", _) => "euros",
        _ => "currency",
    }
}

fn date_milliseconds(value: &Value) -> f64 {
    match value {
        _ if value.is_string() => parse_iso_instant(&value.to_js_string()),
        _ if value.is_object() => value.get_property("__w3cos_date_milliseconds").to_number(),
        _ => value.to_number(),
    }
}

#[derive(Clone, Copy)]
enum TimeZoneSpec {
    Fixed(i64),
    Iana(chrono_tz::Tz),
}

impl TimeZoneSpec {
    fn offset_minutes(self, milliseconds: f64) -> i64 {
        match self {
            Self::Fixed(minutes) => minutes,
            Self::Iana(time_zone) => chrono::Utc
                .timestamp_millis_opt(milliseconds.floor() as i64)
                .single()
                .map(|instant| {
                    time_zone
                        .offset_from_utc_datetime(&instant.naive_utc())
                        .fix()
                        .local_minus_utc() as i64
                        / 60
                })
                .unwrap_or(0),
        }
    }
}

fn parse_time_zone(time_zone: &str) -> Option<TimeZoneSpec> {
    parse_fixed_offset(time_zone)
        .map(TimeZoneSpec::Fixed)
        .or_else(|| time_zone.parse().ok().map(TimeZoneSpec::Iana))
}

fn parse_fixed_offset(value: &str) -> Option<i64> {
    if matches!(value, "UTC" | "Etc/UTC" | "GMT") {
        return Some(0);
    }
    let value = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("GMT"))?;
    if value.is_empty() {
        return Some(0);
    }
    let sign = match value.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let parts: Vec<&str> = value[1..].split(':').collect();
    let hours: i64 = parts.first()?.parse().ok()?;
    let minutes: i64 = match parts.get(1) {
        Some(part) => part.parse().ok()?,
        None => 0,
    };
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 60 + minutes))
}

fn format_date_time(
    milliseconds: f64,
    offset_minutes: i64,
    locale: &str,
    date_style: Option<&str>,
    time_style: Option<&str>,
) -> String {
    let local_seconds = (milliseconds / 1000.0).floor() as i64 + offset_minutes * 60;
    let days = local_seconds.div_euclid(86_400);
    let seconds = local_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3_600;
    let minute = seconds % 3_600 / 60;
    let second = seconds % 60;
    let include_date = date_style.is_some() || time_style.is_none();
    let include_time = time_style.is_some();
    let long_time = matches!(time_style, Some("medium" | "long" | "full"));
    let date = match locale {
        "zh-CN" => format!("{year}年{month}月{day}日"),
        "de-DE" => format!("{day:02}.{month:02}.{year}"),
        "fr-FR" => {
            const MONTHS: [&str; 12] = [
                "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
                "nov.", "déc.",
            ];
            format!("{day} {} {year}", MONTHS[(month - 1) as usize])
        }
        "ja-JP" => format!("{year}/{month:02}/{day:02}"),
        _ => {
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            format!("{} {day}, {year}", MONTHS[(month - 1) as usize])
        }
    };
    let time = if locale != "en-US" {
        if long_time {
            format!("{hour:02}:{minute:02}:{second:02}")
        } else {
            format!("{hour:02}:{minute:02}")
        }
    } else {
        let period = if hour < 12 { "AM" } else { "PM" };
        let display_hour = match hour % 12 {
            0 => 12,
            hour => hour,
        };
        if long_time {
            format!("{display_hour}:{minute:02}:{second:02} {period}")
        } else {
            format!("{display_hour}:{minute:02} {period}")
        }
    };
    match (include_date, include_time) {
        (true, true) if matches!(locale, "zh-CN" | "ja-JP") => format!("{date} {time}"),
        (true, true) => format!("{date}, {time}"),
        (true, false) => date,
        (false, true) => time,
        (false, false) => date,
    }
}

fn parse_iso_instant(value: &str) -> f64 {
    let value = value.trim();
    let Some((date, time_and_zone)) = value.split_once('T').or_else(|| value.split_once(' '))
    else {
        return f64::NAN;
    };
    let mut date_parts = date.split('-');
    let (Some(year), Some(month), Some(day)) = (
        date_parts.next().and_then(|part| part.parse::<i64>().ok()),
        date_parts.next().and_then(|part| part.parse::<u32>().ok()),
        date_parts.next().and_then(|part| part.parse::<u32>().ok()),
    ) else {
        return f64::NAN;
    };
    let (time, offset_minutes) = if let Some(time) = time_and_zone.strip_suffix('Z') {
        (time, 0)
    } else if let Some(index) = time_and_zone[1..].rfind(['+', '-']).map(|index| index + 1) {
        let sign = if time_and_zone.as_bytes()[index] == b'+' {
            1
        } else {
            -1
        };
        let offset = &time_and_zone[index + 1..];
        let mut offset_parts = offset.split(':');
        let Some(hours) = offset_parts
            .next()
            .and_then(|part| part.parse::<i64>().ok())
        else {
            return f64::NAN;
        };
        let minutes = offset_parts
            .next()
            .and_then(|part| part.parse::<i64>().ok())
            .unwrap_or(0);
        (&time_and_zone[..index], sign * (hours * 60 + minutes))
    } else {
        (time_and_zone, 0)
    };
    let mut time_parts = time.split(':');
    let (Some(hour), Some(minute), Some(second_part)) = (
        time_parts.next().and_then(|part| part.parse::<i64>().ok()),
        time_parts.next().and_then(|part| part.parse::<i64>().ok()),
        time_parts.next(),
    ) else {
        return f64::NAN;
    };
    let second = second_part.parse::<f64>().unwrap_or(f64::NAN);
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0.0..60.0).contains(&second)
    {
        return f64::NAN;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60
        - offset_minutes * 60;
    (seconds as f64 + second) * 1000.0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let adjusted_month = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

/// `atob(data)` — base64 → binary string (one Latin-1 char per byte).
pub fn atob(args: Vec<Value>) -> Value {
    let input = args
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    match BASE64.decode(cleaned.as_bytes()) {
        Ok(bytes) => Value::from(bytes.iter().map(|b| char::from(*b)).collect::<String>()),
        Err(_) => crate::throw_value(js_error(
            "InvalidCharacterError: the string to be decoded is not correctly encoded",
        )),
    }
}

/// `btoa(data)` — binary string → base64. Chars beyond Latin-1 throw,
/// matching the web platform (encode Unicode via `encodeURIComponent`
/// first, as in JS).
pub fn btoa(args: Vec<Value>) -> Value {
    let input = args
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let mut bytes = Vec::with_capacity(input.len());
    for c in input.chars() {
        if (c as u32) > 0xFF {
            crate::throw_value(js_error(
                "InvalidCharacterError: the string to be encoded contains characters outside of the Latin1 range",
            ));
        }
        bytes.push(c as u8);
    }
    Value::from(BASE64.encode(bytes))
}

// ── structuredClone ────────────────────────────────────────────────────

/// Registers an opaque host object as transferable without exposing
/// forgeable JavaScript properties. `prepare` returns the replacement object;
/// `finalize` detaches the source after the complete clone succeeds.
pub fn register_host_transferable(value: &Value, prepare: Value, finalize: Value) {
    let Some(pointer) = heap_pointer(value) else {
        return;
    };
    HOST_TRANSFERABLES.with(|items| {
        items.borrow_mut().insert(pointer, (prepare, finalize));
    });
}

/// Removes a previously registered host transferable wrapper.
pub fn unregister_host_transferable(value: &Value) {
    if let Some(pointer) = heap_pointer(value) {
        HOST_TRANSFERABLES.with(|items| {
            items.borrow_mut().remove(&pointer);
        });
    }
}

fn host_transfer_hooks(value: &Value) -> Option<(Value, Value)> {
    let pointer = heap_pointer(value)?;
    HOST_TRANSFERABLES.with(|items| items.borrow().get(&pointer).cloned())
}

/// `structuredClone(value)` — deep clone with shared references and cycles.
pub fn structured_clone(args: Vec<Value>) -> Value {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let transfer = args
        .get(1)
        .map(|options| options.get_property("transfer").iter().collect::<Vec<_>>())
        .unwrap_or_default();
    for (index, item) in transfer.iter().enumerate() {
        let custom_transfer = host_transfer_hooks(item).is_some();
        if !crate::binary::is_transferable_array_buffer(item) && !custom_transfer {
            crate::throw_value(js_error(
                "DataCloneError: transfer list item is not transferable",
            ));
        }
        if transfer[..index]
            .iter()
            .any(|existing| existing.strict_eq(item))
        {
            crate::throw_value(js_error(
                "DataCloneError: duplicate transferable in transfer list",
            ));
        }
    }
    let mut clones: HashMap<usize, Value> = HashMap::new();
    let mut custom_finalizers = Vec::new();
    for item in &transfer {
        if crate::binary::is_transferable_array_buffer(item) {
            continue;
        }
        let (prepare, finalize) =
            host_transfer_hooks(item).expect("custom transferable validated above");
        let transferred = prepare.call(item.clone(), vec![]);
        let pointer = heap_pointer(item).expect("custom transferables are objects");
        clones.insert(pointer, transferred);
        custom_finalizers.push((item.clone(), finalize));
    }
    let cloned = clone_value(&value, &mut clones);
    for item in transfer {
        if crate::binary::is_transferable_array_buffer(&item) {
            crate::binary::detach_array_buffer(&item);
        }
    }
    for (item, finalize) in custom_finalizers {
        finalize.call(item, vec![]);
    }
    cloned
}

fn heap_pointer(value: &Value) -> Option<usize> {
    match value {
        _ if value.as_array().is_some() => Some(value.as_array().expect("array").identity()),
        _ if value.as_object().is_some() => Some(value.as_object().expect("object").identity()),
        _ => None,
    }
}

fn clone_value(value: &Value, clones: &mut HashMap<usize, Value>) -> Value {
    if value.is_function() {
        crate::throw_value(js_error("DataCloneError: functions cannot be cloned"));
    }
    let pointer = match heap_pointer(value) {
        Some(pointer) => pointer,
        // Primitives copy directly.
        None => return value.clone(),
    };
    if let Some(cloned) = clones.get(&pointer) {
        return cloned.clone();
    }
    if let Some(bigint) = crate::bigint::get(value) {
        let cloned = crate::bigint::parse(&bigint.to_string());
        clones.insert(pointer, cloned.clone());
        return cloned;
    }
    if let Some(descriptor) = crate::binary::clone_descriptor(value) {
        let cloned = match descriptor {
            crate::binary::BinaryCloneDescriptor::ArrayBuffer { shared } => {
                crate::binary::clone_array_buffer(value, shared)
            }
            crate::binary::BinaryCloneDescriptor::TypedArray {
                name,
                buffer,
                offset,
                length,
            } => {
                let buffer = clone_value(&buffer, clones);
                crate::class::construct(
                    &crate::binary::typed_array_class(name),
                    vec![
                        buffer,
                        Value::Number(offset as f64),
                        Value::Number(length as f64),
                    ],
                )
            }
            crate::binary::BinaryCloneDescriptor::DataView {
                buffer,
                offset,
                length,
            } => {
                let buffer = clone_value(&buffer, clones);
                crate::class::construct(
                    &crate::binary::data_view_class(),
                    vec![
                        buffer,
                        Value::Number(offset as f64),
                        Value::Number(length as f64),
                    ],
                )
            }
        };
        clones.insert(pointer, cloned.clone());
        return cloned;
    }

    match value {
        _ if value.as_array().is_some() => {
            let items = value.as_array().expect("array");
            let children = items.borrow().clone();
            let cloned = Value::array(
                (0..children.len())
                    .map(|_| crate::value::array_hole())
                    .collect(),
            );
            clones.insert(pointer, cloned.clone());
            for (index, item) in children.iter().enumerate() {
                if crate::value::is_array_hole(item) {
                    continue;
                }
                cloned.set_property(&index.to_string(), clone_value(item, clones));
            }
            cloned
        }
        _ if value.as_object().is_some() => {
            let object = value.as_object().expect("object");
            let milliseconds = value.get_property("__w3cos_date_milliseconds");
            if !milliseconds.is_undefined() {
                let cloned = date_value(milliseconds.to_number());
                clones.insert(pointer, cloned.clone());
                return cloned;
            }
            if let Some(name) = structured_error_name(value) {
                let cloned = Value::object(HashMap::from([
                    ("name".to_string(), Value::string(name)),
                    ("message".to_string(), value.get_property("message")),
                    ("stack".to_string(), value.get_property("stack")),
                ]));
                crate::class::set_prototype_of(
                    &cloned,
                    &crate::error_class(name).get_property("prototype"),
                );
                clones.insert(pointer, cloned.clone());
                let cause = value.get_property("cause");
                if !cause.is_undefined() {
                    cloned.set_property("cause", clone_value(&cause, clones));
                }
                if name == "AggregateError" {
                    cloned
                        .set_property("errors", clone_value(&value.get_property("errors"), clones));
                }
                return cloned;
            }
            if crate::class::instance_of(value, &dom_exception_class()) {
                let cloned = dom_exception_instance(
                    &value.get_property("message").to_js_string(),
                    &value.get_property("name").to_js_string(),
                );
                clones.insert(pointer, cloned.clone());
                cloned.set_property("stack", value.get_property("stack"));
                return cloned;
            }
            if crate::class::instance_of(value, &image_data_class()) {
                let cloned_data = clone_value(&value.get_property("data"), clones);
                let cloned = image_data_value(
                    cloned_data,
                    value.get_property("width").to_u32(),
                    value.get_property("height").to_u32(),
                    &value.get_property("colorSpace").to_js_string(),
                );
                clones.insert(pointer, cloned.clone());
                return cloned;
            }
            if crate::class::instance_of(value, &file_class()) {
                let state = blob_state(value).expect("File instances retain Blob state");
                let cloned = file_value(
                    state.bytes.clone(),
                    state.type_name.clone(),
                    value.get_property("name").to_js_string(),
                    value.get_property("lastModified").to_number(),
                );
                clones.insert(pointer, cloned.clone());
                return cloned;
            }
            if crate::class::instance_of(value, &blob_class()) {
                let state = blob_state(value).expect("Blob instances retain state");
                let cloned = make_blob(
                    state.bytes.clone(),
                    state.type_name.clone(),
                    blob_class().get_property("prototype"),
                );
                clones.insert(pointer, cloned.clone());
                return cloned;
            }
            if let Some((source, flags)) = crate::regexp::parts(value) {
                let cloned = crate::regexp::create(&source, &flags);
                cloned.set_property("lastIndex", value.get_property("lastIndex"));
                clones.insert(pointer, cloned.clone());
                return cloned;
            }
            if let Some(snapshot) = crate::collections::collection_snapshot(value) {
                let cloned = match &snapshot {
                    crate::collections::CollectionSnapshot::Map(_) => {
                        crate::class::construct(&crate::collections::map_class(), vec![])
                    }
                    crate::collections::CollectionSnapshot::Set(_) => {
                        crate::class::construct(&crate::collections::set_class(), vec![])
                    }
                };
                clones.insert(pointer, cloned.clone());
                match snapshot {
                    crate::collections::CollectionSnapshot::Map(entries) => {
                        for (key, item) in entries {
                            cloned.call_method(
                                "set",
                                vec![clone_value(&key, clones), clone_value(&item, clones)],
                            );
                        }
                    }
                    crate::collections::CollectionSnapshot::Set(values) => {
                        for item in values {
                            cloned.call_method("add", vec![clone_value(&item, clones)]);
                        }
                    }
                }
                return cloned;
            }

            let cloned = Value::object(HashMap::new());
            clones.insert(pointer, cloned.clone());
            let keys = object.borrow().keys();
            for key in keys {
                let child = object.borrow().get_direct(&key);
                cloned.set_property(&key, clone_value(&child, clones));
            }
            cloned
        }
        _ => unreachable!(),
    }
}

/// Returns the standard Error subclass represented by a runtime value.
///
/// Storage/message codecs use this to retain Error prototype identity.
pub fn structured_error_name(value: &Value) -> Option<&'static str> {
    [
        "AggregateError",
        "EvalError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "TypeError",
        "URIError",
        "Error",
    ]
    .into_iter()
    .find(|name| crate::class::instance_of(value, &crate::error_class(name)))
}

// ── Percent encoding (application/x-www-form-urlencoded) ───────────────

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = |b: u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                match (bytes.get(i + 1), bytes.get(i + 2)) {
                    (Some(&hi), Some(&lo)) => match (hex(hi), hex(lo)) {
                        (Some(hi), Some(lo)) => {
                            out.push(hi * 16 + lo);
                            i += 2;
                        }
                        _ => out.push(b'%'),
                    },
                    _ => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── URLSearchParams ────────────────────────────────────────────────────

type PairList = Rc<RefCell<Vec<(String, String)>>>;

thread_local! {
    static URL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static URL_SEARCH_PARAMS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static URL_PATTERN_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OBJECT_URL_SEQUENCE: Cell<u64> = const { Cell::new(1) };
    static OBJECT_URLS: RefCell<HashMap<String, Rc<BlobState>>> = RefCell::new(HashMap::new());
}

fn web_constructor_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    construct: fn(Vec<Value>) -> Value,
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |this, args| {
            if this.is_undefined() {
                crate::throw_value(js_error(&format!(
                    "TypeError: Class constructor {name} cannot be invoked without 'new'"
                )));
            }
            construct(args)
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn url_class() -> Value {
    let class = web_constructor_class(&URL_CLASS, "URL", url_new);
    let prototype = class.get_property("prototype");
    for property in [
        "hash",
        "host",
        "hostname",
        "href",
        "origin",
        "password",
        "pathname",
        "port",
        "protocol",
        "search",
        "searchParams",
        "toJSON",
        "toString",
        "username",
    ] {
        prototype.set_property(property, Value::Undefined);
    }
    if class.get_property("canParse").is_undefined() {
        class.set_property(
            "canParse",
            Value::function(|_, args| Value::Bool(url_parts_from_args(&args).is_ok())),
        );
        class.set_property(
            "parse",
            Value::function(|_, args| match url_parts_from_args(&args) {
                Ok(parts) => url_value(parts),
                Err(_) => Value::Null,
            }),
        );
        class.set_property(
            "createObjectURL",
            Value::function(|_, args| {
                let source = args.first().cloned().unwrap_or(Value::Undefined);
                let Some(state) = blob_state(&source) else {
                    crate::throw_value(Value::object(HashMap::from([
                        ("name".into(), Value::string("TypeError")),
                        (
                            "message".into(),
                            Value::string("URL.createObjectURL requires a Blob or File"),
                        ),
                    ])));
                };
                let id = OBJECT_URL_SEQUENCE.with(|sequence| {
                    let id = sequence.get();
                    sequence.set(id.saturating_add(1));
                    id
                });
                let url = format!("blob:w3cos/{id}");
                OBJECT_URLS.with(|urls| {
                    urls.borrow_mut().insert(url.clone(), state);
                });
                Value::string(&url)
            }),
        );
        class.set_property(
            "revokeObjectURL",
            Value::function(|_, args| {
                let url = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                OBJECT_URLS.with(|urls| {
                    urls.borrow_mut().remove(&url);
                });
                Value::Undefined
            }),
        );
    }
    class
}

pub fn url_search_params_class() -> Value {
    let class = web_constructor_class(
        &URL_SEARCH_PARAMS_CLASS,
        "URLSearchParams",
        url_search_params_new,
    );
    let prototype = class.get_property("prototype");
    for member in [
        "append", "delete", "entries", "forEach", "get", "getAll", "has", "keys", "set", "size",
        "sort", "toString", "values",
    ] {
        prototype.set_property(member, Value::Undefined);
    }
    class
}

const URL_PATTERN_COMPONENTS: [&str; 8] = [
    "protocol", "username", "password", "hostname", "port", "pathname", "search", "hash",
];
static URL_PATTERN_COMPLEX_WARNING: Once = Once::new();

#[derive(Clone)]
struct UrlPatternParts {
    values: HashMap<String, String>,
    ignore_case: bool,
}

fn pattern_value(parts: &UrlPatternParts, name: &str) -> String {
    parts
        .values
        .get(name)
        .cloned()
        .unwrap_or_else(|| "*".to_string())
}

fn split_pattern_suffix(input: &str) -> (&str, String, String) {
    let (before_hash, hash) = match input.split_once('#') {
        Some((before, value)) => (before, value.to_string()),
        None => (input, "*".to_string()),
    };
    let (before_search, search) = match before_hash.split_once('?') {
        Some((before, value)) => (before, value.to_string()),
        None => (before_hash, "*".to_string()),
    };
    (before_search, search, hash)
}

fn parse_url_pattern_string(input: &str, base: Option<&str>) -> Result<UrlPatternParts, String> {
    let mut values = HashMap::new();
    let (main, search, hash) = split_pattern_suffix(input);
    values.insert("search".into(), search);
    values.insert("hash".into(), hash);

    if let Some((protocol, after_scheme)) = main.split_once("://") {
        values.insert("protocol".into(), protocol.trim_end_matches(':').into());
        let (authority, pathname) = split_authority(after_scheme);
        let host_port = match authority.rsplit_once('@') {
            Some((userinfo, host_port)) => {
                let (username, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
                values.insert("username".into(), username.into());
                values.insert("password".into(), password.into());
                host_port
            }
            None => authority,
        };
        let (hostname, port) = host_port
            .rsplit_once(':')
            .filter(|(_, port)| !port.contains(']'))
            .unwrap_or((host_port, ""));
        values.insert("hostname".into(), hostname.into());
        values.insert("port".into(), port.into());
        values.insert(
            "pathname".into(),
            if pathname.is_empty() {
                "/".into()
            } else {
                pathname.into()
            },
        );
    } else {
        let base = base.ok_or_else(|| {
            "A relative URLPattern string requires an absolute baseURL".to_string()
        })?;
        let base_parts = parse_url(base, None)?;
        values.insert(
            "protocol".into(),
            base_parts.protocol.trim_end_matches(':').into(),
        );
        values.insert("username".into(), base_parts.username);
        values.insert("password".into(), base_parts.password);
        values.insert("hostname".into(), base_parts.hostname);
        values.insert("port".into(), base_parts.port);
        let pathname = if main.starts_with('/') {
            main.to_string()
        } else {
            let base_dir = base_parts
                .pathname
                .rfind('/')
                .map(|index| &base_parts.pathname[..=index])
                .unwrap_or("/");
            format!("{base_dir}{main}")
        };
        values.insert("pathname".into(), pathname);
    }
    for name in URL_PATTERN_COMPONENTS {
        values.entry(name.into()).or_insert_with(|| "*".into());
    }
    Ok(UrlPatternParts {
        values,
        ignore_case: false,
    })
}

fn url_pattern_parts(args: &[Value]) -> Result<UrlPatternParts, String> {
    let input = args.first().cloned().unwrap_or(Value::Undefined);
    let second = args.get(1).cloned().unwrap_or(Value::Undefined);
    let (mut parts, options) = if input.is_object() {
        let mut values = HashMap::new();
        for name in URL_PATTERN_COMPONENTS {
            let value = input.get_property(name);
            values.insert(
                name.into(),
                if value.is_undefined() {
                    "*".into()
                } else {
                    value.to_js_string()
                },
            );
        }
        let base = input.get_property("baseURL");
        if !base.is_undefined() {
            let base = parse_url(&base.to_js_string(), None)?;
            for (name, value) in [
                ("protocol", base.protocol.trim_end_matches(':').to_string()),
                ("username", base.username),
                ("password", base.password),
                ("hostname", base.hostname),
                ("port", base.port),
                ("pathname", base.pathname),
                ("search", base.search.trim_start_matches('?').to_string()),
                ("hash", base.hash.trim_start_matches('#').to_string()),
            ] {
                if pattern_value(
                    &UrlPatternParts {
                        values: values.clone(),
                        ignore_case: false,
                    },
                    name,
                ) == "*"
                {
                    values.insert(name.into(), value);
                }
            }
        }
        (
            UrlPatternParts {
                values,
                ignore_case: false,
            },
            second,
        )
    } else {
        let base = if second.is_nullish() || second.is_object() {
            None
        } else {
            Some(second.to_js_string())
        };
        (
            parse_url_pattern_string(&input.to_js_string(), base.as_deref())?,
            args.get(2).cloned().unwrap_or(Value::Undefined),
        )
    };
    if options.is_object() {
        parts.ignore_case = options.get_property("ignoreCase").to_bool();
    }
    Ok(parts)
}

fn component_regex(
    pattern: &str,
    component: &str,
    ignore_case: bool,
) -> Result<regex::Regex, String> {
    let mut source = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut wildcard = 0usize;
    while index < chars.len() {
        match chars[index] {
            '*' => {
                source.push_str(&format!("(?P<w{wildcard}>.*)"));
                wildcard += 1;
                index += 1;
            }
            ':' => {
                let start = index + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                if end == start {
                    source.push_str("\\:");
                    index += 1;
                    continue;
                }
                let name: String = chars[start..end].iter().collect();
                let matcher = match component {
                    "pathname" => "[^/]+",
                    "hostname" => "[^.]+",
                    _ => ".+?",
                };
                if end < chars.len() && chars[end] == '(' {
                    URL_PATTERN_COMPLEX_WARNING.call_once(|| {
                        eprintln!(
                            "[w3cos] warning: URLPattern custom regular-expression groups are \
                             accepted as compatibility syntax but currently use the component's \
                             default segment matcher"
                        );
                    });
                    let mut depth = 1usize;
                    end += 1;
                    while end < chars.len() && depth > 0 {
                        match chars[end] {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        end += 1;
                    }
                }
                source.push_str(&format!("(?P<{name}>{matcher})"));
                index = end;
            }
            value => {
                source.push_str(&regex::escape(&value.to_string()));
                index += 1;
            }
        }
    }
    source.push('$');
    regex::RegexBuilder::new(&source)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|error| error.to_string())
}

fn input_url_pattern_parts(
    args: &[Value],
) -> Result<(HashMap<String, String>, Vec<Value>), String> {
    let input = args.first().cloned().unwrap_or(Value::Undefined);
    if input.is_object() {
        let mut values = HashMap::new();
        for name in URL_PATTERN_COMPONENTS {
            let value = input.get_property(name);
            values.insert(
                name.into(),
                if value.is_undefined() {
                    String::new()
                } else {
                    value.to_js_string()
                },
            );
        }
        return Ok((values, vec![input]));
    }
    let input_string = input.to_js_string();
    let base = args.get(1).cloned().unwrap_or(Value::Undefined);
    let base_parts = if base.is_nullish() {
        None
    } else {
        Some(parse_url(&base.to_js_string(), None)?)
    };
    let parsed = parse_url(&input_string, base_parts.as_ref())?;
    let values = HashMap::from([
        (
            "protocol".into(),
            parsed.protocol.trim_end_matches(':').into(),
        ),
        ("username".into(), parsed.username),
        ("password".into(), parsed.password),
        ("hostname".into(), parsed.hostname),
        ("port".into(), parsed.port),
        ("pathname".into(), parsed.pathname),
        (
            "search".into(),
            parsed.search.trim_start_matches('?').into(),
        ),
        ("hash".into(), parsed.hash.trim_start_matches('#').into()),
    ]);
    let mut inputs = vec![Value::string(&input_string)];
    if !base.is_nullish() {
        inputs.push(base);
    }
    Ok((values, inputs))
}

fn url_pattern_match(this: &Value, args: &[Value], include_result: bool) -> Value {
    let (values, inputs) = match input_url_pattern_parts(args) {
        Ok(value) => value,
        Err(_) => {
            return if include_result {
                Value::Null
            } else {
                Value::Bool(false)
            };
        }
    };
    let ignore_case = this.get_property("__w3cos_ignore_case").to_bool();
    let mut component_results = HashMap::new();
    for component in URL_PATTERN_COMPONENTS {
        let pattern = this
            .get_property(&format!("__w3cos_pattern_{component}"))
            .to_js_string();
        let regex = match component_regex(&pattern, component, ignore_case) {
            Ok(regex) => regex,
            Err(_) => {
                return if include_result {
                    Value::Null
                } else {
                    Value::Bool(false)
                };
            }
        };
        let input = values.get(component).cloned().unwrap_or_default();
        let Some(captures) = regex.captures(&input) else {
            return if include_result {
                Value::Null
            } else {
                Value::Bool(false)
            };
        };
        if include_result {
            let mut groups = HashMap::new();
            let mut wildcard = 0usize;
            for name in regex.capture_names().flatten() {
                let key = name
                    .strip_prefix('w')
                    .map(|_| {
                        let key = wildcard.to_string();
                        wildcard += 1;
                        key
                    })
                    .unwrap_or_else(|| name.to_string());
                groups.insert(
                    key,
                    captures
                        .name(name)
                        .map(|value| Value::string(value.as_str()))
                        .unwrap_or(Value::Undefined),
                );
            }
            component_results.insert(
                component.into(),
                Value::object(HashMap::from([
                    ("input".into(), Value::string(&input)),
                    ("groups".into(), Value::object(groups)),
                ])),
            );
        }
    }
    if !include_result {
        return Value::Bool(true);
    }
    component_results.insert("inputs".into(), Value::array(inputs));
    Value::object(component_results)
}

pub fn url_pattern_class() -> Value {
    let class = web_constructor_class(&URL_PATTERN_CLASS, "URLPattern", url_pattern_new);
    let prototype = class.get_property("prototype");
    prototype.set_property("hasRegExpGroups", Value::Undefined);
    if prototype.get_property("test").is_undefined() {
        for component in URL_PATTERN_COMPONENTS {
            // Keep the public accessor name visible to reflection while the
            // runtime-specific getter stores the executable accessor body.
            prototype.set_property(component, Value::Undefined);
            prototype.set_property(
                &format!("__w3cos_getter_{component}"),
                Value::function(move |this, _| {
                    this.get_property(&format!("__w3cos_pattern_{component}"))
                }),
            );
        }
        prototype.set_property(
            "test",
            Value::function(|this, args| url_pattern_match(&this, &args, false)),
        );
        prototype.set_property(
            "exec",
            Value::function(|this, args| url_pattern_match(&this, &args, true)),
        );
    }
    class
}

pub fn url_pattern_new(args: Vec<Value>) -> Value {
    let parts = match url_pattern_parts(&args) {
        Ok(parts) => parts,
        Err(message) => crate::throw_value(js_error(&format!(
            "TypeError: Failed to construct 'URLPattern': {message}"
        ))),
    };
    let instance = Value::object(HashMap::new());
    for component in URL_PATTERN_COMPONENTS {
        instance.set_property(
            &format!("__w3cos_pattern_{component}"),
            Value::string(&pattern_value(&parts, component)),
        );
    }
    instance.set_property("__w3cos_ignore_case", Value::Bool(parts.ignore_case));
    crate::class::set_prototype_of(&instance, &url_pattern_class().get_property("prototype"));
    instance
}

/// Resolve a live object URL to its immutable Blob bytes and media type.
pub fn object_url_resource(url: &str) -> Option<(Vec<u8>, String)> {
    OBJECT_URLS.with(|urls| {
        urls.borrow()
            .get(url)
            .map(|state| (state.bytes.clone(), state.type_name.clone()))
    })
}

/// Parse `a=1&b=2` (leading `?` tolerated) into decoded pairs.
fn parse_query(query: &str) -> Vec<(String, String)> {
    let query = query.strip_prefix('?').unwrap_or(query);
    if query.is_empty() {
        return Vec::new();
    }
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn serialize_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Build a `URLSearchParams` value. `linked_parts`, when present, is the
/// owning `URL`'s parts: mutations re-serialize into its `search`.
fn params_value(
    pairs: Vec<(String, String)>,
    linked_parts: Option<Rc<RefCell<UrlParts>>>,
) -> Value {
    let state: PairList = Rc::new(RefCell::new(pairs));
    let params = Value::object(HashMap::new());
    let weak_self = params
        .as_object()
        .map(|object| object.downgrade())
        .expect("URLSearchParams object");

    /// After-mutation hook: push the new serialization into the URL.
    macro_rules! sync_to_url {
        ($state:expr, $link:expr) => {{
            if let Some(parts) = &$link {
                let pairs = $state.borrow();
                let search = serialize_pairs(&pairs);
                parts.borrow_mut().search = if search.is_empty() {
                    String::new()
                } else {
                    format!("?{search}")
                };
            }
        }};
    }

    {
        let state = state.clone();
        params.set_property(
            "get",
            Value::function(move |_, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                state
                    .borrow()
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| Value::from(v.clone()))
                    .unwrap_or(Value::Null)
            }),
        );
    }
    {
        let state = state.clone();
        params.set_property(
            "getAll",
            Value::function(move |_, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                Value::array(
                    state
                        .borrow()
                        .iter()
                        .filter(|(k, _)| *k == key)
                        .map(|(_, v)| Value::from(v.clone()))
                        .collect(),
                )
            }),
        );
    }
    {
        let state = state.clone();
        let link = linked_parts.clone();
        params.set_property(
            "set",
            Value::function(move |_, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let value = args
                    .get(1)
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                state.borrow_mut().retain(|(k, _)| *k != key);
                state.borrow_mut().push((key, value));
                sync_to_url!(state, link);
                Value::Undefined
            }),
        );
    }
    {
        let state = state.clone();
        let link = linked_parts.clone();
        params.set_property(
            "append",
            Value::function(move |_, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let value = args
                    .get(1)
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                state.borrow_mut().push((key, value));
                sync_to_url!(state, link);
                Value::Undefined
            }),
        );
    }
    {
        let state = state.clone();
        params.set_property(
            "has",
            Value::function(move |_, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                Value::Bool(state.borrow().iter().any(|(k, _)| *k == key))
            }),
        );
    }
    {
        let state = state.clone();
        let link = linked_parts.clone();
        params.set_property(
            "delete",
            Value::function(move |_, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                state.borrow_mut().retain(|(k, _)| *k != key);
                sync_to_url!(state, link);
                Value::Undefined
            }),
        );
    }
    {
        let state = state.clone();
        params.set_property(
            "toString",
            Value::function(move |_, _| Value::from(serialize_pairs(&state.borrow()))),
        );
    }
    {
        let state = state.clone();
        let link = linked_parts.clone();
        params.set_property(
            "sort",
            Value::function(move |_, _| {
                state
                    .borrow_mut()
                    .sort_by(|left, right| left.0.cmp(&right.0));
                sync_to_url!(state, link);
                Value::Undefined
            }),
        );
    }
    {
        let state = state.clone();
        params.set_property(
            "forEach",
            Value::function(move |_, args| {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                let receiver = weak_self.upgrade_value().unwrap_or(Value::Undefined);
                // Snapshot so the callback may mutate the params without
                // tripping the RefCell borrow.
                let snapshot = state.borrow().clone();
                for (key, value) in snapshot {
                    callback.call(
                        Value::Undefined,
                        vec![
                            Value::from(value.clone()),
                            Value::from(key.clone()),
                            receiver.clone(),
                        ],
                    );
                }
                Value::Undefined
            }),
        );
    }
    {
        let state = state.clone();
        params.set_property(
            "__w3cos_getter_size",
            Value::function(move |_, _| Value::Number(state.borrow().len() as f64)),
        );
    }
    for (name, projection) in [
        ("keys", 0_u8),
        ("values", 1_u8),
        ("entries", 2_u8),
        ("__w3cosIterableSnapshot", 2_u8),
    ] {
        let state = state.clone();
        params.set_property(
            name,
            Value::function(move |_, _| {
                Value::array(
                    state
                        .borrow()
                        .iter()
                        .map(|(key, value)| match projection {
                            0 => Value::string(key),
                            1 => Value::string(value),
                            _ => Value::array(vec![Value::string(key), Value::string(value)]),
                        })
                        .collect(),
                )
            }),
        );
    }
    crate::class::set_prototype_of(
        &params,
        &url_search_params_class().get_property("prototype"),
    );
    params
}

/// `new URLSearchParams(init)` — init from a query string, an array of
/// `[key, value]` pairs, or a plain object.
pub fn url_search_params_new(args: Vec<Value>) -> Value {
    let init = args.first().cloned().unwrap_or(Value::Undefined);
    let pairs = match &init {
        _ if init.is_nullish() => Vec::new(),
        _ if init.is_string() => parse_query(&init.to_js_string()),
        _ if init.as_array().is_some() => init
            .as_array()
            .expect("array")
            .borrow()
            .iter()
            .map(|pair| {
                if let Some(entry) = pair.as_array() {
                    let entry = entry.borrow();
                    (
                        entry
                            .first()
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                        entry
                            .get(1)
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                    )
                } else {
                    (pair.to_js_string(), String::new())
                }
            })
            .collect(),
        _ if init.as_object().is_some() => {
            let object = init.as_object().expect("object");
            let object = object.borrow();
            object
                .keys()
                .into_iter()
                .map(|key| (key.clone(), object.get_direct(&key).to_js_string()))
                .collect()
        }
        other => parse_query(&other.to_js_string()),
    };
    params_value(pairs, None)
}

// ── URL ────────────────────────────────────────────────────────────────

/// Parsed URL components (all strings, JS-property-shaped: `protocol`
/// keeps its colon, `search`/`hash` keep their `?`/`#`).
#[derive(Clone, Default)]
struct UrlParts {
    protocol: String,
    username: String,
    password: String,
    hostname: String,
    port: String,
    pathname: String,
    search: String,
    hash: String,
}

impl UrlParts {
    fn has_authority(&self) -> bool {
        !self.hostname.is_empty()
    }

    fn host(&self) -> String {
        if self.port.is_empty() {
            self.hostname.clone()
        } else {
            format!("{}:{}", self.hostname, self.port)
        }
    }

    fn origin(&self) -> String {
        if self.has_authority() {
            format!("{}//{}", self.protocol, self.host())
        } else {
            "null".to_string()
        }
    }

    fn href(&self) -> String {
        let mut out = self.protocol.clone();
        if self.has_authority() {
            out.push_str("//");
            if !self.username.is_empty() {
                out.push_str(&self.username);
                if !self.password.is_empty() {
                    out.push(':');
                    out.push_str(&self.password);
                }
                out.push('@');
            }
            out.push_str(&self.host());
        }
        out.push_str(&self.pathname);
        out.push_str(&self.search);
        out.push_str(&self.hash);
        out
    }
}

/// Schemes that get an authority-based `origin` and a default port.
fn default_port(protocol: &str) -> Option<&'static str> {
    match protocol {
        "http:" | "ws:" => Some("80"),
        "https:" | "wss:" => Some("443"),
        "ftp:" => Some("21"),
        _ => None,
    }
}

/// Resolve `.` / `..` segments in an absolute path (RFC 3986 §5.2.4-ish).
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    let joined = segments.join("/");
    // Preserve a trailing slash implied by a trailing "." / "..".
    if (path.ends_with("/.") || path.ends_with("/..")) && !joined.ends_with('/') {
        format!("{joined}/")
    } else {
        joined
    }
}

/// Minimal RFC 3986 parse + relative resolution against `base`.
fn parse_url(input: &str, base: Option<&UrlParts>) -> Result<UrlParts, String> {
    let input = input.trim();
    // Split off fragment and query first — they never contain the scheme.
    let (before_hash, hash) = match input.split_once('#') {
        Some((rest, fragment)) => (rest, format!("#{fragment}")),
        None => (input, String::new()),
    };
    let (before_query, search) = match before_hash.split_once('?') {
        Some((rest, query)) => (rest, format!("?{query}")),
        None => (before_hash, String::new()),
    };

    let scheme_end = before_query.find(':').filter(|&end| {
        let candidate = &before_query[..end];
        !candidate.is_empty()
            && candidate.starts_with(|c: char| c.is_ascii_alphabetic())
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });

    let Some(scheme_end) = scheme_end else {
        // Relative reference: needs a base to resolve against.
        let base = base.ok_or_else(|| format!("'{input}' is not an absolute URL"))?;
        let mut parts = base.clone();
        parts.hash = hash;
        if let Some(authority) = before_query.strip_prefix("//") {
            // Scheme-relative: keep the base scheme, re-parse the authority.
            let (host_part, path_part) = split_authority(authority);
            parse_authority(&mut parts, host_part);
            parts.pathname = normalize_path(&ensure_leading_slash(path_part));
            parts.search = search;
        } else if before_query.is_empty() {
            // Empty reference or query/fragment-only.
            if !search.is_empty() {
                parts.search = search;
            }
        } else if before_query.starts_with('/') {
            parts.pathname = normalize_path(before_query);
            parts.search = search;
        } else {
            let base_dir = match base.pathname.rfind('/') {
                Some(index) => &base.pathname[..=index],
                None => "",
            };
            parts.pathname = normalize_path(&format!("{base_dir}{before_query}"));
            parts.search = search;
        }
        return Ok(parts);
    };

    let mut parts = UrlParts {
        protocol: format!("{}:", before_query[..scheme_end].to_ascii_lowercase()),
        hash,
        search,
        ..UrlParts::default()
    };
    let rest = &before_query[scheme_end + 1..];
    if let Some(after_slashes) = rest.strip_prefix("//") {
        let (authority, path) = split_authority(after_slashes);
        parse_authority(&mut parts, authority);
        parts.pathname = if path.is_empty() {
            if parts.has_authority() {
                "/".to_string()
            } else {
                String::new()
            }
        } else {
            normalize_path(&ensure_leading_slash(path))
        };
    } else {
        // No authority: opaque path (`mailto:x`, `about:blank`, ...).
        parts.pathname = rest.to_string();
    }
    Ok(parts)
}

/// Split `authority/path...` at the first `/`.
fn split_authority(after_slashes: &str) -> (&str, &str) {
    match after_slashes.find('/') {
        Some(index) => (&after_slashes[..index], &after_slashes[index..]),
        None => (after_slashes, ""),
    }
}

fn ensure_leading_slash(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Parse `[user[:pass]@]host[:port]` into `parts`.
fn parse_authority(parts: &mut UrlParts, authority: &str) {
    let host_port = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            match userinfo.split_once(':') {
                Some((user, password)) => {
                    parts.username = percent_decode(user);
                    parts.password = percent_decode(password);
                }
                None => parts.username = percent_decode(userinfo),
            }
            host_port
        }
        None => authority,
    };
    match host_port.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            parts.hostname = host.to_ascii_lowercase();
            let default = default_port(&parts.protocol).unwrap_or("");
            parts.port = if port == default {
                String::new()
            } else {
                port.to_string()
            };
        }
        _ => parts.hostname = host_port.to_ascii_lowercase(),
    }
}

/// Build the JS-facing `URL` value around shared parts.
fn url_value(parts: UrlParts) -> Value {
    let shared = Rc::new(RefCell::new(parts));
    let url = Value::object(HashMap::new());

    // Data-backed fields go through the __w3cos_getter_/__w3cos_setter_
    // convention so href/toString always reflect the latest writes.
    macro_rules! accessor {
        ($name:literal, $field:ident) => {{
            let parts = shared.clone();
            url.set_property(
                concat!("__w3cos_getter_", $name),
                Value::function(move |_, _| Value::from(parts.borrow().$field.clone())),
            );
            let parts = shared.clone();
            url.set_property(
                concat!("__w3cos_setter_", $name),
                Value::function(move |_, args| {
                    let value = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    parts.borrow_mut().$field = value;
                    Value::Undefined
                }),
            );
        }};
    }
    accessor!("username", username);
    accessor!("password", password);
    accessor!("hostname", hostname);
    accessor!("port", port);

    // protocol: normalize on write (lowercase, trailing colon).
    {
        let parts = shared.clone();
        url.set_property(
            "__w3cos_getter_protocol",
            Value::function(move |_, _| Value::from(parts.borrow().protocol.clone())),
        );
        let parts = shared.clone();
        url.set_property(
            "__w3cos_setter_protocol",
            Value::function(move |_, args| {
                let mut value = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                value = value.trim_end_matches(':').to_ascii_lowercase();
                parts.borrow_mut().protocol = format!("{value}:");
                Value::Undefined
            }),
        );
    }
    // host: computed from hostname + port; writing splits at the colon.
    {
        let parts = shared.clone();
        url.set_property(
            "__w3cos_getter_host",
            Value::function(move |_, _| Value::from(parts.borrow().host())),
        );
        let parts = shared.clone();
        url.set_property(
            "__w3cos_setter_host",
            Value::function(move |_, args| {
                let value = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let mut borrowed = parts.borrow_mut();
                match value.split_once(':') {
                    Some((host, port)) => {
                        borrowed.hostname = host.to_ascii_lowercase();
                        borrowed.port = port.to_string();
                    }
                    None => borrowed.hostname = value.to_ascii_lowercase(),
                }
                Value::Undefined
            }),
        );
    }
    // pathname: writes get a leading slash when the URL has an authority.
    {
        let parts = shared.clone();
        url.set_property(
            "__w3cos_getter_pathname",
            Value::function(move |_, _| Value::from(parts.borrow().pathname.clone())),
        );
        let parts = shared.clone();
        url.set_property(
            "__w3cos_setter_pathname",
            Value::function(move |_, args| {
                let value = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let mut borrowed = parts.borrow_mut();
                borrowed.pathname = if borrowed.has_authority() {
                    ensure_leading_slash(&value)
                } else {
                    value
                };
                Value::Undefined
            }),
        );
    }
    // search / hash: writes gain their `?` / `#` prefix.
    for (name, field, prefix) in [("search", "search", '?'), ("hash", "hash", '#')] {
        let parts = shared.clone();
        url.set_property(
            &format!("__w3cos_getter_{name}"),
            Value::function(move |_, _| {
                let borrowed = parts.borrow();
                let value = match field {
                    "search" => &borrowed.search,
                    _ => &borrowed.hash,
                };
                Value::from(value.clone())
            }),
        );
        let parts = shared.clone();
        url.set_property(
            &format!("__w3cos_setter_{name}"),
            Value::function(move |_, args| {
                let value = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let value = if value.is_empty() || value.starts_with(prefix) {
                    value
                } else {
                    format!("{prefix}{value}")
                };
                let mut borrowed = parts.borrow_mut();
                match field {
                    "search" => borrowed.search = value,
                    _ => borrowed.hash = value,
                }
                Value::Undefined
            }),
        );
    }
    // origin: read-only, computed.
    {
        let parts = shared.clone();
        url.set_property(
            "__w3cos_getter_origin",
            Value::function(move |_, _| Value::from(parts.borrow().origin())),
        );
    }
    // href: full serialization; writing re-parses as an absolute URL.
    {
        let parts = shared.clone();
        url.set_property(
            "__w3cos_getter_href",
            Value::function(move |_, _| Value::from(parts.borrow().href())),
        );
        let parts = shared.clone();
        url.set_property(
            "__w3cos_setter_href",
            Value::function(move |_, args| {
                let value = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                match parse_url(&value, None) {
                    Ok(parsed) => *parts.borrow_mut() = parsed,
                    Err(message) => {
                        crate::throw_value(js_error(&format!("TypeError: Invalid URL: {message}")));
                    }
                }
                Value::Undefined
            }),
        );
    }
    {
        let parts = shared.clone();
        url.set_property(
            "toString",
            Value::function(move |_, _| Value::from(parts.borrow().href())),
        );
    }
    // searchParams shares the parts back: mutations rewrite `search`.
    let query_pairs = parse_query(shared.borrow().search.strip_prefix('?').unwrap_or(""));
    url.set_property("searchParams", params_value(query_pairs, Some(shared)));
    crate::class::set_prototype_of(&url, &url_class().get_property("prototype"));
    url
}

/// `new URL(url[, base])` — minimal RFC 3986 parse; relative references
/// resolve against `base`. Unparseable input throws a JS `TypeError`.
pub fn url_new(args: Vec<Value>) -> Value {
    match url_parts_from_args(&args) {
        Ok(parts) => url_value(parts),
        Err(message) => crate::throw_value(js_error(&format!("TypeError: Invalid URL: {message}"))),
    }
}

fn url_parts_from_args(args: &[Value]) -> Result<UrlParts, String> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let base_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    let base_parts = if base_arg.is_nullish() {
        None
    } else {
        Some(
            parse_url(&base_arg.to_js_string(), None)
                .map_err(|message| format!("Invalid base URL: {message}"))?,
        )
    };
    parse_url(&input, base_parts.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    /// Test helper: unwrap a caught JS-exception payload into its Value.
    fn payload_value(payload: Box<dyn std::any::Any + Send>) -> Value {
        crate::promise::payload_to_value(payload)
    }

    // ── atob / btoa ──

    #[test]
    fn base64_roundtrip() {
        let encoded = btoa(vec![Value::string("hello world")]);
        assert_eq!(encoded.to_js_string(), "aGVsbG8gd29ybGQ=");
        assert_eq!(atob(vec![encoded]).to_js_string(), "hello world");
    }

    #[test]
    fn btoa_rejects_non_latin1() {
        // JS parity: btoa("€") throws; Unicode goes through
        // encodeURIComponent-style escaping first — i.e. btoa over the
        // UTF-8 bytes read as a Latin-1 string ("\u{E2}\u{82}\u{AC}" for "€").
        let outcome = catch_unwind(AssertUnwindSafe(|| btoa(vec![Value::string("€")])));
        assert!(outcome.is_err());
        let utf8_bytes_as_latin1 = "\u{E2}\u{82}\u{AC}";
        let encoded = btoa(vec![Value::string(utf8_bytes_as_latin1)]);
        assert_eq!(atob(vec![encoded]).to_js_string(), utf8_bytes_as_latin1);
    }

    #[test]
    fn atob_rejects_garbage() {
        let outcome = catch_unwind(AssertUnwindSafe(|| atob(vec![Value::string("###")])));
        assert!(outcome.is_err());
    }

    // ── structuredClone ──

    #[test]
    fn structured_clone_primitives_and_rejects_functions() {
        assert_eq!(structured_clone(vec![Value::Number(5.0)]).to_number(), 5.0);
        let f = Value::function(|_, _| Value::Number(1.0));
        let payload = catch_unwind(AssertUnwindSafe(|| structured_clone(vec![f])))
            .expect_err("functions are not structured-cloneable");
        assert!(
            payload_value(payload)
                .get_property("message")
                .to_js_string()
                .contains("functions cannot be cloned")
        );
    }

    #[test]
    fn structured_clone_deep_copy_is_independent() {
        let mut inner_props = HashMap::new();
        inner_props.insert("n".to_string(), Value::Number(1.0));
        let inner = Value::object(inner_props);
        let original = Value::array(vec![inner, Value::string("s")]);

        let cloned = structured_clone(vec![original.clone()]);
        original
            .get_property("0")
            .set_property("n", Value::Number(99.0));

        assert_eq!(cloned.get_property("0").get_property("n").to_number(), 1.0);
        assert_eq!(cloned.get_property("1").to_js_string(), "s");
        assert_ne!(cloned.get_property("0"), original.get_property("0"));
    }

    #[test]
    fn structured_clone_preserves_shared_structure() {
        let shared = Value::array(vec![Value::Number(7.0)]);
        let original = Value::array(vec![shared.clone(), shared]);
        let cloned = structured_clone(vec![original]);
        assert_eq!(cloned.get_property("0"), cloned.get_property("1"));
        assert_eq!(cloned.get_property("0").to_js_string(), "7");
    }

    #[test]
    fn structured_clone_preserves_cycles() {
        let object = Value::object(HashMap::new());
        object.set_property("me", object.clone());
        let cloned = structured_clone(vec![object.clone()]);
        assert_ne!(cloned, object);
        assert_eq!(cloned.get_property("me"), cloned);
    }

    #[test]
    fn structured_clone_preserves_date_regexp_map_and_set_types() {
        let bigint = crate::bigint::parse("9007199254740993123456789");
        let cloned_bigint = structured_clone(vec![bigint]);
        assert_eq!(
            crate::bigint::get(&cloned_bigint)
                .expect("BigInt should remain cloneable")
                .to_string(),
            "9007199254740993123456789"
        );

        let date = date_value(1_784_795_415_000.0);
        let cloned_date = structured_clone(vec![date]);
        assert_eq!(
            cloned_date.call_method("getTime", vec![]).to_number(),
            1_784_795_415_000.0
        );

        let regexp = crate::regexp::create("a+", "gi");
        regexp.set_property("lastIndex", Value::Number(3.0));
        let cloned_regexp = structured_clone(vec![regexp]);
        assert!(crate::class::instance_of(
            &cloned_regexp,
            &crate::regexp::regexp_class()
        ));
        assert_eq!(cloned_regexp.get_property("source").to_js_string(), "a+");
        assert_eq!(cloned_regexp.get_property("flags").to_js_string(), "gi");
        assert_eq!(cloned_regexp.get_property("lastIndex").to_number(), 3.0);

        let map = crate::class::construct(&crate::collections::map_class(), vec![]);
        map.call_method("set", vec![Value::string("self"), map.clone()]);
        let cloned_map = structured_clone(vec![map]);
        assert!(crate::class::instance_of(
            &cloned_map,
            &crate::collections::map_class()
        ));
        assert_eq!(
            cloned_map.call_method("get", vec![Value::string("self")]),
            cloned_map
        );

        let set = crate::class::construct(
            &crate::collections::set_class(),
            vec![Value::array(vec![Value::string("item")])],
        );
        let cloned_set = structured_clone(vec![set]);
        assert!(crate::class::instance_of(
            &cloned_set,
            &crate::collections::set_class()
        ));
        assert!(
            cloned_set
                .call_method("has", vec![Value::string("item")])
                .to_bool()
        );
    }

    #[test]
    fn structured_clone_preserves_error_types_causes_and_aggregate_entries() {
        let typed = crate::error_instance("TypeError", vec![Value::string("bad input")]);
        typed.set_property("cause", typed.clone());
        let aggregate = crate::error_instance(
            "AggregateError",
            vec![
                Value::array(vec![typed.clone(), typed]),
                Value::string("many"),
                Value::object(HashMap::from([(
                    "cause".to_string(),
                    Value::string("root"),
                )])),
            ],
        );

        let cloned = structured_clone(vec![aggregate.clone()]);
        assert_ne!(cloned, aggregate);
        assert!(crate::class::instance_of(
            &cloned,
            &crate::error_class("AggregateError")
        ));
        assert!(crate::class::instance_of(
            &cloned,
            &crate::error_class("Error")
        ));
        assert_eq!(cloned.get_property("message").to_js_string(), "many");
        assert_eq!(cloned.get_property("cause").to_js_string(), "root");
        let errors = cloned.get_property("errors");
        assert_eq!(errors.get_property("0"), errors.get_property("1"));
        let cloned_typed = errors.get_property("0");
        assert!(crate::class::instance_of(
            &cloned_typed,
            &crate::error_class("TypeError")
        ));
        assert_eq!(cloned_typed.get_property("cause"), cloned_typed);
    }

    #[test]
    fn dom_exception_has_legacy_codes_and_is_structured_cloneable() {
        let exception = dom_exception_instance("stopped", "AbortError");
        assert!(crate::class::instance_of(
            &exception,
            &dom_exception_class()
        ));
        assert_eq!(exception.get_property("code").to_number(), 20.0);
        assert_eq!(
            exception.call_method("toString", vec![]).to_js_string(),
            "AbortError: stopped"
        );
        assert_eq!(
            dom_exception_class()
                .get_property("DATA_CLONE_ERR")
                .to_number(),
            25.0
        );
        assert_eq!(
            dom_exception_class()
                .get_property("prototype")
                .get_property("ABORT_ERR")
                .to_number(),
            20.0
        );

        let cloned = structured_clone(vec![exception.clone()]);
        assert_ne!(cloned, exception);
        assert!(crate::class::instance_of(&cloned, &dom_exception_class()));
        assert_eq!(cloned.get_property("name").to_js_string(), "AbortError");
        assert_eq!(cloned.get_property("message").to_js_string(), "stopped");
        assert_eq!(cloned.get_property("code").to_number(), 20.0);
    }

    #[test]
    fn structured_clone_preserves_image_data_and_shared_pixel_storage() {
        let data = crate::class::construct(
            &crate::binary::typed_array_class("Uint8ClampedArray"),
            vec![Value::Number(4.0)],
        );
        data.set_property("0", Value::Number(255.0));
        let image = image_data_value(data.clone(), 1, 1, "display-p3");
        let cloned = structured_clone(vec![Value::object(HashMap::from([
            ("image".to_string(), image.clone()),
            ("data".to_string(), data),
        ]))]);
        let cloned_image = cloned.get_property("image");
        assert_ne!(cloned_image, image);
        assert!(crate::class::instance_of(
            &cloned_image,
            &image_data_class()
        ));
        assert_eq!(
            cloned_image.get_property("colorSpace").to_js_string(),
            "display-p3"
        );
        assert_eq!(
            cloned_image.get_property("data"),
            cloned.get_property("data")
        );
        assert_eq!(
            cloned_image
                .get_property("data")
                .get_property("0")
                .to_number(),
            255.0
        );
    }

    #[test]
    fn structured_clone_preserves_blob_and_file_bytes_and_metadata() {
        let blob = crate::class::construct(
            &blob_class(),
            vec![
                Value::array(vec![Value::string("hello")]),
                Value::object(HashMap::from([(
                    "type".to_string(),
                    Value::string("Text/Plain"),
                )])),
            ],
        );
        let file = crate::class::construct(
            &file_class(),
            vec![
                Value::array(vec![blob.clone()]),
                Value::string("note.txt"),
                Value::object(HashMap::from([
                    ("type".to_string(), Value::string("text/custom")),
                    ("lastModified".to_string(), Value::Number(123.0)),
                ])),
            ],
        );
        let cloned = structured_clone(vec![Value::array(vec![blob.clone(), file.clone()])]);
        let cloned_blob = cloned.get_property("0");
        let cloned_file = cloned.get_property("1");
        assert_ne!(cloned_blob, blob);
        assert_ne!(cloned_file, file);
        assert!(crate::class::instance_of(&cloned_blob, &blob_class()));
        assert!(crate::class::instance_of(&cloned_file, &file_class()));
        assert!(crate::class::instance_of(&cloned_file, &blob_class()));
        assert_eq!(
            cloned_blob.call_method("text", vec![]).to_js_string(),
            "hello"
        );
        assert_eq!(
            cloned_blob.get_property("type").to_js_string(),
            "text/plain"
        );
        assert_eq!(
            cloned_file.call_method("text", vec![]).to_js_string(),
            "hello"
        );
        assert_eq!(
            cloned_file.get_property("type").to_js_string(),
            "text/custom"
        );
        assert_eq!(cloned_file.get_property("name").to_js_string(), "note.txt");
        assert_eq!(cloned_file.get_property("lastModified").to_number(), 123.0);
    }

    #[test]
    fn structured_clone_preserves_binary_views_and_backing_relationships() {
        let buffer = crate::class::construct(
            &crate::binary::array_buffer_class(),
            vec![Value::Number(8.0)],
        );
        let bytes = crate::class::construct(
            &crate::binary::typed_array_class("Uint8Array"),
            vec![buffer.clone(), Value::Number(2.0), Value::Number(3.0)],
        );
        bytes.set_property("0", Value::Number(7.0));
        bytes.set_property("1", Value::Number(8.0));
        let data_view = crate::class::construct(
            &crate::binary::data_view_class(),
            vec![buffer.clone(), Value::Number(2.0), Value::Number(3.0)],
        );
        let cloned = structured_clone(vec![Value::object(HashMap::from([
            ("bytes".into(), bytes.clone()),
            ("view".into(), data_view),
        ]))]);
        let cloned_bytes = cloned.get_property("bytes");
        let cloned_view = cloned.get_property("view");
        assert!(crate::class::instance_of(
            &cloned_bytes,
            &crate::binary::typed_array_class("Uint8Array")
        ));
        assert!(crate::class::instance_of(
            &cloned_view,
            &crate::binary::data_view_class()
        ));
        assert_eq!(cloned_bytes.get_property("byteOffset").to_number(), 2.0);
        assert_eq!(cloned_bytes.get_property("length").to_number(), 3.0);
        assert_eq!(
            cloned_bytes.get_property("buffer"),
            cloned_view.get_property("buffer")
        );
        assert_ne!(cloned_bytes.get_property("buffer"), buffer);
        bytes.set_property("0", Value::Number(99.0));
        assert_eq!(cloned_bytes.get_property("0").to_number(), 7.0);
        assert_eq!(
            cloned_view
                .call_method("getUint8", vec![Value::Number(1.0)])
                .to_number(),
            8.0
        );

        let shared = crate::class::construct(
            &crate::binary::shared_array_buffer_class(),
            vec![Value::Number(2.0)],
        );
        let original_shared_view = crate::class::construct(
            &crate::binary::typed_array_class("Uint8Array"),
            vec![shared.clone()],
        );
        let cloned_shared = structured_clone(vec![shared.clone()]);
        let cloned_shared_view = crate::class::construct(
            &crate::binary::typed_array_class("Uint8Array"),
            vec![cloned_shared.clone()],
        );
        assert_ne!(cloned_shared, shared);
        cloned_shared_view.set_property("0", Value::Number(42.0));
        assert_eq!(original_shared_view.get_property("0").to_number(), 42.0);
    }

    #[test]
    fn structured_clone_transfers_and_detaches_array_buffers() {
        let buffer = crate::class::construct(
            &crate::binary::array_buffer_class(),
            vec![Value::Number(4.0)],
        );
        let original_view = crate::class::construct(
            &crate::binary::typed_array_class("Uint8Array"),
            vec![buffer.clone()],
        );
        original_view.set_property("0", Value::Number(11.0));
        let transferred = structured_clone(vec![
            buffer.clone(),
            Value::object(HashMap::from([(
                "transfer".into(),
                Value::array(vec![buffer.clone()]),
            )])),
        ]);
        assert_eq!(buffer.get_property("byteLength").to_number(), 0.0);
        assert_eq!(original_view.get_property("length").to_number(), 0.0);
        assert_eq!(transferred.get_property("byteLength").to_number(), 4.0);
        let transferred_view = crate::class::construct(
            &crate::binary::typed_array_class("Uint8Array"),
            vec![transferred],
        );
        assert_eq!(transferred_view.get_property("0").to_number(), 11.0);

        let duplicate = crate::class::construct(
            &crate::binary::array_buffer_class(),
            vec![Value::Number(1.0)],
        );
        let duplicate_outcome = catch_unwind(AssertUnwindSafe(|| {
            structured_clone(vec![
                Value::Null,
                Value::object(HashMap::from([(
                    "transfer".into(),
                    Value::array(vec![duplicate.clone(), duplicate.clone()]),
                )])),
            ])
        }));
        assert!(duplicate_outcome.is_err());
        assert_eq!(duplicate.get_property("byteLength").to_number(), 1.0);

        let shared = crate::class::construct(
            &crate::binary::shared_array_buffer_class(),
            vec![Value::Number(1.0)],
        );
        let shared_outcome = catch_unwind(AssertUnwindSafe(|| {
            structured_clone(vec![
                Value::Null,
                Value::object(HashMap::from([(
                    "transfer".into(),
                    Value::array(vec![shared]),
                )])),
            ])
        }));
        assert!(shared_outcome.is_err());
    }

    #[test]
    fn structured_clone_custom_transfer_finalizes_only_after_clone_succeeds() {
        let finalized = Rc::new(Cell::new(false));
        let transferable = Value::object(HashMap::new());
        let prepare = Value::function(|_, _| {
            Value::object(HashMap::from([("moved".to_string(), Value::Bool(true))]))
        });
        let finalized_for_hook = Rc::clone(&finalized);
        let finalize = Value::function(move |_, _| {
            finalized_for_hook.set(true);
            Value::Undefined
        });
        register_host_transferable(&transferable, prepare, finalize);
        let options = Value::object(HashMap::from([(
            "transfer".to_string(),
            Value::array(vec![transferable.clone()]),
        )]));
        let failed = catch_unwind(AssertUnwindSafe(|| {
            structured_clone(vec![
                Value::object(HashMap::from([
                    ("port".to_string(), transferable.clone()),
                    (
                        "invalid".to_string(),
                        Value::function(|_, _| Value::Undefined),
                    ),
                ])),
                options.clone(),
            ])
        }));
        assert!(failed.is_err());
        assert!(
            !finalized.get(),
            "a failed clone must not detach transferables"
        );

        let cloned = structured_clone(vec![
            Value::object(HashMap::from([("port".to_string(), transferable)])),
            options,
        ]);
        assert!(finalized.get());
        assert!(cloned.get_property("port").get_property("moved").to_bool());
    }

    // ── URL ──

    #[test]
    fn url_parses_full_form() {
        let url = url_new(vec![Value::string(
            "https://user:pw@Example.COM:8080/p/a?x=1&y=2#frag",
        )]);
        assert!(crate::class::instance_of(&url, &url_class()));
        assert!(crate::class::instance_of(
            &url.get_property("searchParams"),
            &url_search_params_class()
        ));
        assert_eq!(url.get_property("protocol").to_js_string(), "https:");
        assert_eq!(url.get_property("username").to_js_string(), "user");
        assert_eq!(url.get_property("password").to_js_string(), "pw");
        assert_eq!(url.get_property("hostname").to_js_string(), "example.com");
        assert_eq!(url.get_property("port").to_js_string(), "8080");
        assert_eq!(url.get_property("host").to_js_string(), "example.com:8080");
        assert_eq!(url.get_property("pathname").to_js_string(), "/p/a");
        assert_eq!(url.get_property("search").to_js_string(), "?x=1&y=2");
        assert_eq!(url.get_property("hash").to_js_string(), "#frag");
        assert_eq!(
            url.get_property("origin").to_js_string(),
            "https://example.com:8080"
        );
        assert_eq!(
            url.get_property("href").to_js_string(),
            "https://user:pw@example.com:8080/p/a?x=1&y=2#frag"
        );
        assert_eq!(
            url.call_method("toString", vec![]).to_js_string(),
            url.get_property("href").to_js_string()
        );
    }

    #[test]
    fn url_parses_monaco_style_urls() {
        let file = url_new(vec![Value::string("file:///Users/x/project/src/main.ts")]);
        assert_eq!(file.get_property("protocol").to_js_string(), "file:");
        assert_eq!(
            file.get_property("pathname").to_js_string(),
            "/Users/x/project/src/main.ts"
        );
        assert_eq!(file.get_property("origin").to_js_string(), "null");

        let cdn = url_new(vec![Value::string(
            "https://cdn.jsdelivr.net/npm/monaco-editor@0.52/vs/editor/editor.main.js",
        )]);
        assert_eq!(
            cdn.get_property("hostname").to_js_string(),
            "cdn.jsdelivr.net"
        );
        assert_eq!(cdn.get_property("port").to_js_string(), "");
        assert_eq!(
            cdn.get_property("pathname").to_js_string(),
            "/npm/monaco-editor@0.52/vs/editor/editor.main.js"
        );
        // Default port dropped from the href.
        let default_port = url_new(vec![Value::string("https://example.com:443/a")]);
        assert_eq!(
            default_port.get_property("href").to_js_string(),
            "https://example.com/a"
        );
    }

    #[test]
    fn url_resolves_relative_against_base() {
        let url = url_new(vec![
            Value::string("../b/c.js?d=4#e"),
            Value::string("https://example.com/a/d/e.js"),
        ]);
        assert_eq!(url.get_property("pathname").to_js_string(), "/a/b/c.js");
        assert_eq!(url.get_property("search").to_js_string(), "?d=4");
        assert_eq!(url.get_property("hash").to_js_string(), "#e");
        assert_eq!(
            url.get_property("origin").to_js_string(),
            "https://example.com"
        );

        let absolute_path = url_new(vec![
            Value::string("/root.js"),
            Value::string("https://example.com/a/b"),
        ]);
        assert_eq!(
            absolute_path.get_property("pathname").to_js_string(),
            "/root.js"
        );

        let scheme_relative = url_new(vec![
            Value::string("//other.com/x"),
            Value::string("https://example.com/"),
        ]);
        assert_eq!(
            scheme_relative.get_property("hostname").to_js_string(),
            "other.com"
        );
        assert_eq!(
            scheme_relative.get_property("protocol").to_js_string(),
            "https:"
        );
    }

    #[test]
    fn url_writes_stay_consistent_with_href() {
        let url = url_new(vec![Value::string("https://example.com/a?x=1")]);
        url.set_property("pathname", Value::string("/b"));
        url.set_property("hash", Value::string("sec"));
        assert_eq!(
            url.get_property("href").to_js_string(),
            "https://example.com/b?x=1#sec"
        );
    }

    #[test]
    fn url_invalid_throws() {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            url_new(vec![Value::string("not a url")])
        }));
        let payload = outcome.expect_err("invalid URL must throw");
        let error = payload_value(payload);
        assert!(
            error
                .get_property("message")
                .to_js_string()
                .contains("Invalid URL")
        );
    }

    #[test]
    fn url_pattern_matches_named_and_wildcard_groups() {
        let pattern = url_pattern_new(vec![Value::string(
            "https://*.example.com/books/:id?view=*",
        )]);
        assert_eq!(
            pattern.get_property("pathname").to_js_string(),
            "/books/:id"
        );
        assert!(
            pattern
                .call_method(
                    "test",
                    vec![Value::string("https://docs.example.com/books/42?view=full")],
                )
                .to_bool()
        );
        assert!(
            !pattern
                .call_method(
                    "test",
                    vec![Value::string(
                        "https://docs.example.com/authors/42?view=full"
                    )],
                )
                .to_bool()
        );

        let result = pattern.call_method(
            "exec",
            vec![Value::string("https://docs.example.com/books/42?view=full")],
        );
        assert_eq!(
            result
                .get_property("pathname")
                .get_property("groups")
                .get_property("id")
                .to_js_string(),
            "42"
        );
        assert_eq!(
            result
                .get_property("hostname")
                .get_property("groups")
                .get_property("0")
                .to_js_string(),
            "docs"
        );
    }

    #[test]
    fn url_pattern_resolves_relative_input_and_honors_ignore_case() {
        let options = Value::object(HashMap::from([("ignoreCase".into(), Value::Bool(true))]));
        let pattern = url_pattern_new(vec![
            Value::string("/Users/:name"),
            Value::string("https://example.com/base/"),
            options,
        ]);
        assert!(
            pattern
                .call_method("test", vec![Value::string("https://EXAMPLE.com/users/Ada")],)
                .to_bool()
        );
        assert!(
            pattern
                .call_method(
                    "exec",
                    vec![
                        Value::string("../Users/Bob"),
                        Value::string("https://example.com/base/page"),
                    ],
                )
                .is_object()
        );
    }

    // ── URLSearchParams ──

    #[test]
    fn params_crud_and_roundtrip() {
        let params = url_search_params_new(vec![Value::string("a=1&b=2&a=3")]);
        assert_eq!(
            params
                .call_method("get", vec![Value::string("a")])
                .to_js_string(),
            "1"
        );
        assert_eq!(
            params
                .call_method("getAll", vec![Value::string("a")])
                .to_js_string(),
            "1,3"
        );
        assert!(
            params
                .call_method("has", vec![Value::string("b")])
                .to_bool()
        );
        assert!(
            params
                .call_method("get", vec![Value::string("missing")])
                .is_null()
        );

        params.call_method("set", vec![Value::string("b"), Value::string("9")]);
        params.call_method("append", vec![Value::string("c"), Value::string("x y")]);
        params.call_method("delete", vec![Value::string("a")]);
        assert!(
            !params
                .call_method("has", vec![Value::string("a")])
                .to_bool()
        );
        assert_eq!(
            params.call_method("toString", vec![]).to_js_string(),
            "b=9&c=x+y"
        );

        // Parse its own serialization back — same content.
        let again = url_search_params_new(vec![params.call_method("toString", vec![])]);
        assert_eq!(
            again
                .call_method("get", vec![Value::string("c")])
                .to_js_string(),
            "x y"
        );
    }

    #[test]
    fn params_sort_and_foreach() {
        let params = url_search_params_new(vec![Value::string("b=2&a=1&c=3")]);
        params.call_method("sort", vec![]);
        assert_eq!(
            params.call_method("toString", vec![]).to_js_string(),
            "a=1&b=2&c=3"
        );
        assert_eq!(params.get_property("size").to_u32(), 3);
        assert_eq!(params.call_method("keys", vec![]).to_js_string(), "a,b,c");
        assert_eq!(params.call_method("values", vec![]).to_js_string(), "1,2,3");
        assert_eq!(
            params.call_method("entries", vec![]).to_js_string(),
            "a,1,b,2,c,3"
        );
        assert_eq!(
            Value::array(params.iter().collect()).to_js_string(),
            "a,1,b,2,c,3"
        );

        let log = Rc::new(RefCell::new(Vec::new()));
        let log2 = log.clone();
        params.call_method(
            "forEach",
            vec![Value::function(move |_, args| {
                log2.borrow_mut().push(format!(
                    "{}={}",
                    args[1].to_js_string(),
                    args[0].to_js_string()
                ));
                Value::Undefined
            })],
        );
        assert_eq!(
            log.borrow().as_slice(),
            &["a=1".to_string(), "b=2".to_string(), "c=3".to_string()]
        );
    }

    #[test]
    fn params_init_from_pairs_and_object() {
        let from_pairs = url_search_params_new(vec![Value::array(vec![Value::array(vec![
            Value::string("k"),
            Value::string("v"),
        ])])]);
        assert_eq!(
            from_pairs
                .call_method("get", vec![Value::string("k")])
                .to_js_string(),
            "v"
        );

        let mut props = HashMap::new();
        props.insert("q".to_string(), Value::string("monaco"));
        let from_object = url_search_params_new(vec![Value::object(props)]);
        assert_eq!(
            from_object.call_method("toString", vec![]).to_js_string(),
            "q=monaco"
        );
    }

    #[test]
    fn url_search_params_are_live_linked() {
        let url = url_new(vec![Value::string("https://example.com/?a=1")]);
        let params = url.get_property("searchParams");
        assert_eq!(
            params
                .call_method("get", vec![Value::string("a")])
                .to_js_string(),
            "1"
        );
        params.call_method("append", vec![Value::string("b"), Value::string("2")]);
        assert_eq!(url.get_property("search").to_js_string(), "?a=1&b=2");
        assert_eq!(
            url.get_property("href").to_js_string(),
            "https://example.com/?a=1&b=2"
        );
    }

    #[test]
    fn percent_encoding_roundtrip() {
        assert_eq!(percent_encode("a b+c/d"), "a+b%2Bc%2Fd");
        assert_eq!(percent_decode("a+b%2Bc%2Fd"), "a b+c/d");
        assert_eq!(percent_decode("%E2%82%AC"), "€");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn text_decoder_decodes_utf16_code_units() {
        let decoder =
            crate::class::construct(&text_decoder_class(), vec![Value::string("UTF-16LE")]);
        let units = Value::array(
            "<div>✓</div>"
                .encode_utf16()
                .map(|unit| Value::Number(unit as f64))
                .collect(),
        );
        assert_eq!(
            decoder.call_method("decode", vec![units]).to_js_string(),
            "<div>✓</div>"
        );
        assert!(crate::class::instance_of(&decoder, &text_decoder_class()));
        assert_eq!(decoder.get_property("encoding"), Value::string("utf-16le"));
        assert_eq!(decoder.get_property("fatal"), Value::Bool(false));
        assert_eq!(decoder.get_property("ignoreBOM"), Value::Bool(false));
    }

    #[test]
    fn text_decoder_honors_utf8_bom_and_options() {
        let decoder = crate::class::construct(
            &text_decoder_class(),
            vec![
                Value::string("utf8"),
                Value::object(HashMap::from([
                    ("fatal".into(), Value::Bool(true)),
                    ("ignoreBOM".into(), Value::Bool(false)),
                ])),
            ],
        );
        let bytes = crate::binary::typed_array_value(
            [0xef, 0xbb, 0xbf, b'o', b'k']
                .into_iter()
                .map(|byte| Value::Number(byte as f64))
                .collect(),
        );
        assert_eq!(
            decoder.call_method("decode", vec![bytes]),
            Value::string("ok")
        );
        assert_eq!(decoder.get_property("fatal"), Value::Bool(true));
    }

    #[test]
    fn text_decoder_streams_split_bom_utf8_and_utf16_sequences() {
        fn bytes(values: &[u8]) -> Value {
            crate::binary::typed_array_value(
                values
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            )
        }
        let stream = Value::object(HashMap::from([("stream".into(), Value::Bool(true))]));
        let utf8 = crate::class::construct(&text_decoder_class(), vec![]);
        for chunk in [&[0xef][..], &[0xbb][..], &[0xbf, 0xe2][..], &[0x9c][..]] {
            assert_eq!(
                utf8.call_method("decode", vec![bytes(chunk), stream.clone()]),
                Value::string("")
            );
        }
        assert_eq!(
            utf8.call_method("decode", vec![bytes(&[0x93, b'!']), stream.clone()]),
            Value::string("✓!")
        );
        assert_eq!(utf8.call_method("decode", vec![]), Value::string(""));
        assert_eq!(
            utf8.call_method("decode", vec![bytes(&[0xef, 0xbb, 0xbf, b'A'])]),
            Value::string("A")
        );

        let utf16 = crate::class::construct(&text_decoder_class(), vec![Value::string("utf-16le")]);
        assert_eq!(
            utf16.call_method("decode", vec![bytes(&[0x3d, 0xd8, 0x00]), stream.clone()]),
            Value::string("")
        );
        assert_eq!(
            utf16.call_method("decode", vec![bytes(&[0xde]), stream]),
            Value::string("😀")
        );
    }

    #[test]
    fn fatal_text_decoder_defers_incomplete_sequence_error_until_flush() {
        let decoder = crate::class::construct(
            &text_decoder_class(),
            vec![
                Value::string("utf-8"),
                Value::object(HashMap::from([("fatal".into(), Value::Bool(true))])),
            ],
        );
        let partial = crate::binary::typed_array_value(vec![Value::Number(0xe2 as f64)]);
        assert_eq!(
            decoder.call_method(
                "decode",
                vec![
                    partial,
                    Value::object(HashMap::from([("stream".into(), Value::Bool(true))])),
                ],
            ),
            Value::string("")
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                decoder.call_method("decode", vec![]);
            }))
            .is_err()
        );
    }

    #[test]
    fn intl_number_format_supports_grouping_currency_and_fraction_options() {
        let intl = intl_value();
        let number_format = intl.get_property("NumberFormat");
        let currency_options = Value::object(HashMap::from([
            ("style".into(), Value::string("currency")),
            ("currency".into(), Value::string("CNY")),
            ("currencyDisplay".into(), Value::string("narrowSymbol")),
        ]));
        let currency = crate::class::construct(
            &number_format,
            vec![Value::string("zh-CN"), currency_options],
        );
        assert_eq!(
            currency
                .call_method("format", vec![Value::Number(1_234_567.8)])
                .to_js_string(),
            "¥1,234,567.80"
        );

        let decimal_options = Value::object(HashMap::from([(
            "maximumFractionDigits".into(),
            Value::Number(2.0),
        )]));
        let decimal = crate::class::construct(
            &number_format,
            vec![Value::string("en-US"), decimal_options],
        );
        assert_eq!(
            decimal
                .call_method("format", vec![Value::Number(12_345.678)])
                .to_js_string(),
            "12,345.68"
        );
        assert_eq!(
            currency
                .call_method("format", vec![Value::Number(-12.5)])
                .to_js_string(),
            "-¥12.50"
        );
    }

    #[test]
    fn intl_formats_additional_locale_profiles_and_currency_minor_units() {
        let number_format = intl_value().get_property("NumberFormat");
        let euro_options = Value::object(HashMap::from([
            ("style".into(), Value::string("currency")),
            ("currency".into(), Value::string("EUR")),
        ]));
        let german =
            crate::class::construct(&number_format, vec![Value::string("de-AT"), euro_options]);
        assert_eq!(
            german
                .call_method("format", vec![Value::Number(1_234_567.8)])
                .to_js_string(),
            "1.234.567,80 €"
        );
        assert_eq!(
            german
                .call_method("resolvedOptions", vec![])
                .get_property("locale")
                .to_js_string(),
            "de-DE"
        );

        let french = crate::class::construct(
            &number_format,
            vec![
                Value::string("fr-CA"),
                Value::object(HashMap::from([(
                    "maximumFractionDigits".into(),
                    Value::Number(1.0),
                )])),
            ],
        );
        assert_eq!(
            french
                .call_method("format", vec![Value::Number(1_234_567.8)])
                .to_js_string(),
            "1 234 567,8"
        );

        let yen = crate::class::construct(
            &number_format,
            vec![
                Value::string("ja-JP"),
                Value::object(HashMap::from([
                    ("style".into(), Value::string("currency")),
                    ("currency".into(), Value::string("JPY")),
                ])),
            ],
        );
        assert_eq!(
            yen.call_method("format", vec![Value::Number(1234.4)])
                .to_js_string(),
            "¥1,234"
        );

        let date_time_format = intl_value().get_property("DateTimeFormat");
        let japanese = crate::class::construct(
            &date_time_format,
            vec![
                Value::string("ja-JP"),
                Value::object(HashMap::from([
                    ("timeZone".into(), Value::string("Asia/Tokyo")),
                    ("dateStyle".into(), Value::string("medium")),
                    ("timeStyle".into(), Value::string("short")),
                ])),
            ],
        );
        assert_eq!(
            japanese
                .call_method("format", vec![Value::string("2026-07-23T08:30:15Z")])
                .to_js_string(),
            "2026/07/23 17:30"
        );
    }

    #[test]
    fn intl_date_time_format_handles_iso_date_and_shanghai_timezone() {
        let date =
            crate::class::construct(&date_class(), vec![Value::string("2026-07-23T08:30:15Z")]);
        assert_eq!(
            date.call_method("getTime", vec![]),
            Value::Number(1_784_795_415_000.0)
        );
        assert_eq!(
            date.call_method("toISOString", vec![]),
            Value::string("2026-07-23T08:30:15.000Z")
        );

        let options = Value::object(HashMap::from([
            ("timeZone".into(), Value::string("Asia/Shanghai")),
            ("dateStyle".into(), Value::string("medium")),
            ("timeStyle".into(), Value::string("short")),
        ]));
        let date_time_format = intl_value().get_property("DateTimeFormat");
        let formatter =
            crate::class::construct(&date_time_format, vec![Value::string("zh-CN"), options]);
        assert_eq!(
            formatter.call_method("format", vec![date]).to_js_string(),
            "2026年7月23日 16:30"
        );
    }

    #[test]
    fn intl_date_time_format_applies_iana_dst_transitions() {
        let options = Value::object(HashMap::from([
            ("timeZone".into(), Value::string("America/New_York")),
            ("timeStyle".into(), Value::string("short")),
        ]));
        let formatter = crate::class::construct(
            &intl_value().get_property("DateTimeFormat"),
            vec![Value::string("en-US"), options],
        );

        assert_eq!(
            formatter
                .call_method("format", vec![Value::string("2026-03-08T06:30:00Z")])
                .to_js_string(),
            "1:30 AM"
        );
        assert_eq!(
            formatter
                .call_method("format", vec![Value::string("2026-03-08T07:30:00Z")])
                .to_js_string(),
            "3:30 AM"
        );

        let fixed = crate::class::construct(
            &intl_value().get_property("DateTimeFormat"),
            vec![
                Value::string("en-US"),
                Value::object(HashMap::from([
                    ("timeZone".into(), Value::string("UTC+05:30")),
                    ("timeStyle".into(), Value::string("short")),
                ])),
            ],
        );
        assert_eq!(
            fixed
                .call_method("format", vec![Value::string("2026-01-01T00:00:00Z")])
                .to_js_string(),
            "5:30 AM"
        );

        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::class::construct(
                    &intl_value().get_property("DateTimeFormat"),
                    vec![
                        Value::string("en-US"),
                        Value::object(HashMap::from([(
                            "timeZone".into(),
                            Value::string("Mars/Olympus_Mons"),
                        )])),
                    ],
                );
            }))
            .is_err()
        );
    }
}
