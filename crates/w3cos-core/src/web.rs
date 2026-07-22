//! Web platform builtins for the ESM compile pipeline: `atob` / `btoa`,
//! `structuredClone`, `URL`, and `URLSearchParams`.
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
//! `structuredClone` copies own enumerable properties only (no prototype),
//! and clones functions by reference (per the platform's exclusion list).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::Value;
use crate::value::js_error;

// ── atob / btoa ────────────────────────────────────────────────────────

/// Minimal `TextDecoder` constructor used by generated browser code.
/// Typed arrays are represented as arrays of numeric elements, so UTF-16
/// decoding consumes those values directly as code units.
pub fn text_decoder_class() -> Value {
    Value::callable(HashMap::new(), |_this, args| {
        let encoding = args
            .first()
            .cloned()
            .unwrap_or_else(|| Value::string("utf-8"))
            .to_js_string()
            .to_ascii_lowercase()
            .replace('_', "-");
        let decode_encoding = encoding.clone();
        Value::object(HashMap::from([
            ("encoding".to_string(), Value::string(&encoding)),
            (
                "decode".to_string(),
                Value::function(move |_this, args| {
                    let input = args.first().cloned().unwrap_or(Value::Undefined);
                    let numbers: Vec<u16> =
                        input.iter().map(|value| value.to_number() as u16).collect();
                    if decode_encoding.contains("utf-16") {
                        Value::String(String::from_utf16_lossy(&numbers))
                    } else {
                        let bytes: Vec<u8> = numbers.into_iter().map(|value| value as u8).collect();
                        Value::String(String::from_utf8_lossy(&bytes).into_owned())
                    }
                }),
            ),
        ]))
    })
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
        Ok(bytes) => Value::String(bytes.iter().map(|b| char::from(*b)).collect()),
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
    Value::String(BASE64.encode(bytes))
}

// ── structuredClone ────────────────────────────────────────────────────

/// `structuredClone(value)` — deep clone; shared substructures stay shared
/// in the clone, cycles throw.
pub fn structured_clone(args: Vec<Value>) -> Value {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let mut clones: HashMap<usize, CloneSlot> = HashMap::new();
    clone_value(&value, &mut clones)
}

enum CloneSlot {
    /// Currently being cloned — hitting it again means a cycle.
    InProgress,
    Done(Value),
}

fn clone_value(value: &Value, clones: &mut HashMap<usize, CloneSlot>) -> Value {
    let pointer = match value {
        Value::Array(items) => Rc::as_ptr(items) as usize,
        Value::Object(object) => Rc::as_ptr(object) as usize,
        // Primitives copy; functions clone by reference (spec exclusion).
        _ => return value.clone(),
    };
    match clones.get(&pointer) {
        Some(CloneSlot::InProgress) => {
            crate::throw_value(js_error("DataCloneError: cyclic object value"))
        }
        Some(CloneSlot::Done(cloned)) => return cloned.clone(),
        None => {}
    }
    clones.insert(pointer, CloneSlot::InProgress);
    let cloned = match value {
        Value::Array(items) => {
            let children = items
                .borrow()
                .iter()
                .map(|item| clone_value(item, clones))
                .collect();
            Value::array(children)
        }
        Value::Object(object) => {
            let mut properties = HashMap::new();
            let keys = object.borrow().keys();
            for key in keys {
                let child = object.borrow().get_direct(&key);
                properties.insert(key, clone_value(&child, clones));
            }
            Value::object(properties)
        }
        _ => unreachable!(),
    };
    clones.insert(pointer, CloneSlot::Done(cloned.clone()));
    cloned
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
    let weak_self: Weak<RefCell<crate::JsObject>> = match &params {
        Value::Object(object) => Rc::downgrade(object),
        _ => unreachable!(),
    };

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
                    .map(|(_, v)| Value::String(v.clone()))
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
                        .map(|(_, v)| Value::String(v.clone()))
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
            Value::function(move |_, _| Value::String(serialize_pairs(&state.borrow()))),
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
                let receiver = weak_self
                    .upgrade()
                    .map(Value::Object)
                    .unwrap_or(Value::Undefined);
                // Snapshot so the callback may mutate the params without
                // tripping the RefCell borrow.
                let snapshot = state.borrow().clone();
                for (key, value) in snapshot {
                    callback.call(
                        Value::Undefined,
                        vec![
                            Value::String(value.clone()),
                            Value::String(key.clone()),
                            receiver.clone(),
                        ],
                    );
                }
                Value::Undefined
            }),
        );
    }
    params
}

/// `new URLSearchParams(init)` — init from a query string, an array of
/// `[key, value]` pairs, or a plain object.
pub fn url_search_params_new(args: Vec<Value>) -> Value {
    let init = args.first().cloned().unwrap_or(Value::Undefined);
    let pairs = match &init {
        Value::Undefined | Value::Null => Vec::new(),
        Value::String(query) => parse_query(query),
        Value::Array(items) => items
            .borrow()
            .iter()
            .map(|pair| {
                if let Value::Array(entry) = pair {
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
        Value::Object(object) => {
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
                Value::function(move |_, _| Value::String(parts.borrow().$field.clone())),
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
            Value::function(move |_, _| Value::String(parts.borrow().protocol.clone())),
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
            Value::function(move |_, _| Value::String(parts.borrow().host())),
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
            Value::function(move |_, _| Value::String(parts.borrow().pathname.clone())),
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
                Value::String(value.clone())
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
            Value::function(move |_, _| Value::String(parts.borrow().origin())),
        );
    }
    // href: full serialization; writing re-parses as an absolute URL.
    {
        let parts = shared.clone();
        url.set_property(
            "__w3cos_getter_href",
            Value::function(move |_, _| Value::String(parts.borrow().href())),
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
            Value::function(move |_, _| Value::String(parts.borrow().href())),
        );
    }
    // searchParams shares the parts back: mutations rewrite `search`.
    let query_pairs = parse_query(shared.borrow().search.strip_prefix('?').unwrap_or(""));
    url.set_property("searchParams", params_value(query_pairs, Some(shared)));
    url
}

/// `new URL(url[, base])` — minimal RFC 3986 parse; relative references
/// resolve against `base`. Unparseable input throws a JS `TypeError`.
pub fn url_new(args: Vec<Value>) -> Value {
    let input = args
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let base_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    let base_parts = if base_arg.is_nullish() {
        None
    } else {
        match parse_url(&base_arg.to_js_string(), None) {
            Ok(parts) => Some(parts),
            Err(message) => {
                crate::throw_value(js_error(&format!("TypeError: Invalid base URL: {message}")));
            }
        }
    };
    match parse_url(&input, base_parts.as_ref()) {
        Ok(parts) => url_value(parts),
        Err(message) => crate::throw_value(js_error(&format!("TypeError: Invalid URL: {message}"))),
    }
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
    fn structured_clone_primitives_and_functions() {
        assert_eq!(structured_clone(vec![Value::Number(5.0)]).to_number(), 5.0);
        let f = Value::function(|_, _| Value::Number(1.0));
        assert!(structured_clone(vec![f]).is_function());
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
    fn structured_clone_cycle_throws() {
        let object = Value::object(HashMap::new());
        object.set_property("me", object.clone());
        let outcome = catch_unwind(AssertUnwindSafe(|| structured_clone(vec![object])));
        let payload = outcome.expect_err("cycle must throw");
        let error = payload_value(payload);
        assert!(
            error
                .get_property("message")
                .to_js_string()
                .contains("cyclic")
        );
    }

    // ── URL ──

    #[test]
    fn url_parses_full_form() {
        let url = url_new(vec![Value::string(
            "https://user:pw@Example.COM:8080/p/a?x=1&y=2#frag",
        )]);
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
    }
}
