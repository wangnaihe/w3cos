//! Web Crypto `SubtleCrypto` backed by `ring`.
//!
//! Implemented algorithms (pure Rust/asm via `ring`, identical on every
//! platform):
//!
//! - `digest`: SHA-1, SHA-256, SHA-384, SHA-512.
//! - HMAC (SHA-1/256/384/512): `generateKey`, `importKey`/`exportKey` in
//!   `raw` and `jwk`, `sign`, `verify`.
//! - AES-GCM (128/256-bit keys, 96-bit IV, 128-bit tag): `generateKey`,
//!   `importKey`/`exportKey` in `raw` and `jwk`, `encrypt`, `decrypt`,
//!   `wrapKey`, `unwrapKey`.
//! - HKDF and PBKDF2 (SHA-1/256/384/512): `importKey` (`raw`), `deriveBits`,
//!   `deriveKey` into HMAC or AES-GCM keys.
//!
//! Everything else (RSA, ECDSA/ECDH, Ed25519/X25519, AES-CBC/CTR/KW,
//! AES-192, non-96-bit IVs, truncated GCM tags, `spki`/`pkcs8`) rejects
//! `NotSupportedError` instead of returning a fake result.
//!
//! Key material lives in a per-realm registry. A `CryptoKey` object carries
//! only an unguessable handle in a `__w3cos_` slot, so the secret bytes are
//! never reachable as a JavaScript property.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;

use w3cos_core::Value;

type Outcome = Result<Value, Value>;

const KEY_SLOT: &str = "__w3cos_crypto_key";

/// Canonical usage order from the Web Crypto `KeyUsage` enum.
const USAGE_ORDER: [&str; 8] = [
    "encrypt",
    "decrypt",
    "sign",
    "verify",
    "deriveKey",
    "deriveBits",
    "wrapKey",
    "unwrapKey",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn name(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
        }
    }

    fn digest(self) -> &'static ring::digest::Algorithm {
        match self {
            Hash::Sha1 => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
            Hash::Sha256 => &ring::digest::SHA256,
            Hash::Sha384 => &ring::digest::SHA384,
            Hash::Sha512 => &ring::digest::SHA512,
        }
    }

    fn hmac(self) -> ring::hmac::Algorithm {
        match self {
            Hash::Sha1 => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            Hash::Sha256 => ring::hmac::HMAC_SHA256,
            Hash::Sha384 => ring::hmac::HMAC_SHA384,
            Hash::Sha512 => ring::hmac::HMAC_SHA512,
        }
    }

    fn hkdf(self) -> ring::hkdf::Algorithm {
        match self {
            Hash::Sha1 => ring::hkdf::HKDF_SHA1_FOR_LEGACY_USE_ONLY,
            Hash::Sha256 => ring::hkdf::HKDF_SHA256,
            Hash::Sha384 => ring::hkdf::HKDF_SHA384,
            Hash::Sha512 => ring::hkdf::HKDF_SHA512,
        }
    }

    fn pbkdf2(self) -> ring::pbkdf2::Algorithm {
        match self {
            Hash::Sha1 => ring::pbkdf2::PBKDF2_HMAC_SHA1,
            Hash::Sha256 => ring::pbkdf2::PBKDF2_HMAC_SHA256,
            Hash::Sha384 => ring::pbkdf2::PBKDF2_HMAC_SHA384,
            Hash::Sha512 => ring::pbkdf2::PBKDF2_HMAC_SHA512,
        }
    }

    /// HMAC default key length: the hash block size in bits.
    fn block_bits(self) -> u32 {
        match self {
            Hash::Sha1 | Hash::Sha256 => 512,
            Hash::Sha384 | Hash::Sha512 => 1024,
        }
    }

    fn jwk_hmac_alg(self) -> &'static str {
        match self {
            Hash::Sha1 => "HS1",
            Hash::Sha256 => "HS256",
            Hash::Sha384 => "HS384",
            Hash::Sha512 => "HS512",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Hmac(Hash),
    AesGcm,
    Hkdf,
    Pbkdf2,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Hmac(_) => "HMAC",
            Kind::AesGcm => "AES-GCM",
            Kind::Hkdf => "HKDF",
            Kind::Pbkdf2 => "PBKDF2",
        }
    }

    fn allowed_usages(self) -> &'static [&'static str] {
        match self {
            Kind::Hmac(_) => &["sign", "verify"],
            Kind::AesGcm => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
            Kind::Hkdf | Kind::Pbkdf2 => &["deriveKey", "deriveBits"],
        }
    }
}

#[derive(Clone)]
struct KeyRecord {
    kind: Kind,
    secret: Vec<u8>,
    extractable: bool,
    usages: Vec<&'static str>,
}

thread_local! {
    static KEYS: RefCell<HashMap<String, KeyRecord>> = RefCell::new(HashMap::new());
}

/// Drops every key handle created in the current realm.
pub fn reset_realm() {
    KEYS.with(|keys| keys.borrow_mut().clear());
}

// ── Errors ────────────────────────────────────────────────────────────────

fn dom(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn type_error(message: &str) -> Value {
    w3cos_core::error_instance("TypeError", vec![Value::string(message)])
}

fn not_supported(message: &str) -> Value {
    dom("NotSupportedError", message)
}

// ── Argument helpers ──────────────────────────────────────────────────────

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn buffer_source(value: &Value, what: &str) -> Result<Vec<u8>, Value> {
    if let Some(bytes) = w3cos_core::binary::bytes_of(value) {
        return Ok(bytes);
    }
    Err(type_error(&format!(
        "{what} must be an ArrayBuffer or ArrayBufferView"
    )))
}

fn required_member(dict: &Value, member: &str, algorithm: &str) -> Result<Value, Value> {
    let value = dict.get_property(member);
    if value.is_undefined() {
        Err(type_error(&format!(
            "{algorithm} parameters require the '{member}' member"
        )))
    } else {
        Ok(value)
    }
}

/// WebIDL `[EnforceRange] unsigned long` conversion for optional members.
fn optional_unsigned(dict: &Value, member: &str) -> Result<Option<u32>, Value> {
    let value = dict.get_property(member);
    if value.is_undefined() {
        return Ok(None);
    }
    let number = value.to_number();
    if !number.is_finite() || number < 0.0 || number > u32::MAX as f64 {
        return Err(type_error(&format!(
            "'{member}' is outside the unsigned long range"
        )));
    }
    Ok(Some(number.trunc() as u32))
}

/// Returns the algorithm dictionary and its upper-cased name.
fn algorithm_identifier(value: &Value) -> Result<(Value, String), Value> {
    if value.is_string() {
        let name = value.to_js_string();
        let mut dict = HashMap::new();
        dict.insert("name".to_string(), Value::string(&name));
        return Ok((Value::object(dict), name.to_ascii_uppercase()));
    }
    if !value.is_object() {
        return Err(type_error("Algorithm must be a string or an object"));
    }
    let name = value.get_property("name");
    if name.is_undefined() {
        return Err(type_error("Algorithm: the 'name' member is required"));
    }
    Ok((value.clone(), name.to_js_string().to_ascii_uppercase()))
}

fn parse_hash_name(name: &str) -> Option<Hash> {
    match name {
        "SHA-1" => Some(Hash::Sha1),
        "SHA-256" => Some(Hash::Sha256),
        "SHA-384" => Some(Hash::Sha384),
        "SHA-512" => Some(Hash::Sha512),
        _ => None,
    }
}

fn normalize_hash(value: &Value) -> Result<Hash, Value> {
    let (_, name) = algorithm_identifier(value)?;
    parse_hash_name(&name)
        .ok_or_else(|| not_supported(&format!("Unrecognized hash algorithm: {name}")))
}

fn hash_member(dict: &Value, algorithm: &str) -> Result<Hash, Value> {
    normalize_hash(&required_member(dict, "hash", algorithm)?)
}

fn unsupported_algorithm(name: &str, operation: &str) -> Value {
    not_supported(&format!(
        "crypto.subtle.{operation}() does not support algorithm '{name}' in this runtime"
    ))
}

/// Parses a `sequence<KeyUsage>` into canonical order without duplicates.
fn parse_usages(value: &Value) -> Result<Vec<&'static str>, Value> {
    if !value.is_array() && !value.is_object() {
        return Err(type_error(
            "keyUsages must be a sequence of KeyUsage strings",
        ));
    }
    let length = value.get_property("length").to_number();
    if !length.is_finite() || length < 0.0 {
        return Err(type_error(
            "keyUsages must be a sequence of KeyUsage strings",
        ));
    }
    let mut seen = Vec::new();
    for index in 0..length as usize {
        let item = value.get_property(&index.to_string()).to_js_string();
        let Some(usage) = USAGE_ORDER.iter().find(|usage| **usage == item) else {
            return Err(type_error(&format!("'{item}' is not a valid KeyUsage")));
        };
        if !seen.contains(usage) {
            seen.push(*usage);
        }
    }
    Ok(USAGE_ORDER
        .iter()
        .copied()
        .filter(|usage| seen.contains(usage))
        .collect())
}

fn check_usages(kind: Kind, usages: &[&'static str]) -> Result<(), Value> {
    if let Some(bad) = usages
        .iter()
        .find(|usage| !kind.allowed_usages().contains(usage))
    {
        return Err(dom(
            "SyntaxError",
            &format!("Cannot create a {} key with usage '{bad}'", kind.name()),
        ));
    }
    if usages.is_empty() {
        return Err(dom(
            "SyntaxError",
            "Usages cannot be empty when creating a secret key",
        ));
    }
    Ok(())
}

// ── CryptoKey objects ─────────────────────────────────────────────────────

fn new_handle() -> Result<String, Value> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| {
        dom(
            "OperationError",
            "The system random generator is unavailable",
        )
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn algorithm_value(record: &KeyRecord) -> Value {
    let mut props = HashMap::new();
    props.insert("name".to_string(), Value::string(record.kind.name()));
    match record.kind {
        Kind::Hmac(hash) => {
            let mut hash_props = HashMap::new();
            hash_props.insert("name".to_string(), Value::string(hash.name()));
            props.insert("hash".to_string(), Value::object(hash_props));
            props.insert(
                "length".to_string(),
                Value::Number((record.secret.len() * 8) as f64),
            );
        }
        Kind::AesGcm => {
            props.insert(
                "length".to_string(),
                Value::Number((record.secret.len() * 8) as f64),
            );
        }
        Kind::Hkdf | Kind::Pbkdf2 => {}
    }
    Value::object(props)
}

fn crypto_key_value(record: KeyRecord) -> Outcome {
    let handle = new_handle()?;
    let mut props = HashMap::new();
    props.insert("type".to_string(), Value::string("secret"));
    props.insert("extractable".to_string(), Value::Bool(record.extractable));
    props.insert("algorithm".to_string(), algorithm_value(&record));
    props.insert(
        "usages".to_string(),
        Value::array(
            record
                .usages
                .iter()
                .map(|usage| Value::string(usage))
                .collect(),
        ),
    );
    props.insert(KEY_SLOT.to_string(), Value::string(&handle));
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::jsdom::crypto_key_class().get_property("prototype"),
    );
    KEYS.with(|keys| keys.borrow_mut().insert(handle, record));
    Ok(value)
}

fn key_record(value: &Value, what: &str) -> Result<KeyRecord, Value> {
    if value.is_object() {
        let handle = value.get_property(KEY_SLOT);
        if handle.is_string() {
            let handle = handle.to_js_string();
            if let Some(record) = KEYS.with(|keys| keys.borrow().get(&handle).cloned()) {
                return Ok(record);
            }
        }
    }
    Err(type_error(&format!("{what} is not a CryptoKey")))
}

fn require_key(
    value: &Value,
    what: &str,
    algorithm: &str,
    usage: &str,
) -> Result<KeyRecord, Value> {
    let record = key_record(value, what)?;
    if record.kind.name() != algorithm {
        return Err(dom(
            "InvalidAccessError",
            &format!(
                "The requested algorithm {algorithm} does not match the key's algorithm {}",
                record.kind.name()
            ),
        ));
    }
    if !record.usages.contains(&usage) {
        return Err(dom(
            "InvalidAccessError",
            &format!("The key does not support the '{usage}' operation"),
        ));
    }
    Ok(record)
}

// ── Base64url (JWK) ───────────────────────────────────────────────────────

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        let emit = chunk.len() + 1;
        for index in 0..emit {
            out.push(BASE64URL[((n >> (18 - 6 * index)) & 63) as usize] as char);
        }
    }
    out
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = BASE64URL.iter().position(|c| *c == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    // A single leftover sextet cannot encode a byte; reject it as malformed.
    if bits >= 6 {
        return None;
    }
    Some(out)
}

// ── Key import ────────────────────────────────────────────────────────────

/// Interprets `algorithm` for `importKey` / derived-key creation.
fn import_kind(algorithm: &Value, operation: &str) -> Result<(Kind, Value), Value> {
    let (dict, name) = algorithm_identifier(algorithm)?;
    let kind = match name.as_str() {
        "HMAC" => Kind::Hmac(hash_member(&dict, "HMAC")?),
        "AES-GCM" => Kind::AesGcm,
        "HKDF" => Kind::Hkdf,
        "PBKDF2" => Kind::Pbkdf2,
        _ => return Err(unsupported_algorithm(&name, operation)),
    };
    Ok((kind, dict))
}

fn aes_key_bytes_supported(len: usize) -> Result<(), Value> {
    match len {
        16 | 32 => Ok(()),
        24 => Err(not_supported(
            "AES-192 keys are not supported in this runtime",
        )),
        _ => Err(dom(
            "DataError",
            "AES key data must be 128, 192 or 256 bits long",
        )),
    }
}

fn hmac_length_matches(dict: &Value, data_len: usize) -> Result<(), Value> {
    if data_len == 0 {
        return Err(dom("DataError", "HMAC key data must not be empty"));
    }
    if let Some(length) = optional_unsigned(dict, "length")? {
        let data_bits = data_len * 8;
        if length as usize > data_bits || (length as usize) <= data_bits - 8 {
            return Err(dom(
                "DataError",
                "The HMAC 'length' member does not match the key data",
            ));
        }
        if length % 8 != 0 {
            return Err(not_supported(
                "HMAC keys whose length is not a multiple of 8 bits are not supported",
            ));
        }
    }
    Ok(())
}

fn jwk_string(jwk: &Value, member: &str) -> Option<String> {
    let value = jwk.get_property(member);
    if value.is_undefined() {
        None
    } else {
        Some(value.to_js_string())
    }
}

fn import_jwk_secret(
    kind: Kind,
    jwk: &Value,
    extractable: bool,
    usages: &[&'static str],
) -> Result<Vec<u8>, Value> {
    if !jwk.is_object() || w3cos_core::binary::bytes_of(jwk).is_some() {
        return Err(type_error("JWK key data must be a JsonWebKey dictionary"));
    }
    if jwk_string(jwk, "kty").as_deref() != Some("oct") {
        return Err(dom("DataError", "The JWK 'kty' member must be \"oct\""));
    }
    let Some(k) = jwk_string(jwk, "k") else {
        return Err(dom("DataError", "The JWK 'k' member is required"));
    };
    let secret =
        base64url_decode(&k).ok_or_else(|| dom("DataError", "The JWK 'k' member is malformed"))?;
    let expected_use = match kind {
        Kind::Hmac(_) => "sig",
        _ => "enc",
    };
    if !usages.is_empty() {
        if let Some(found) = jwk_string(jwk, "use") {
            if found != expected_use {
                return Err(dom(
                    "DataError",
                    &format!("The JWK 'use' member must be \"{expected_use}\""),
                ));
            }
        }
    }
    let key_ops = jwk.get_property("key_ops");
    if !key_ops.is_undefined() {
        let ops = parse_usages(&key_ops)?;
        if usages.iter().any(|usage| !ops.contains(usage)) {
            return Err(dom(
                "DataError",
                "The requested usages are not allowed by the JWK 'key_ops' member",
            ));
        }
    }
    let ext = jwk.get_property("ext");
    if !ext.is_undefined() && !ext.to_bool() && extractable {
        return Err(dom(
            "DataError",
            "A JWK with ext=false cannot be imported as extractable",
        ));
    }
    let alg = jwk_string(jwk, "alg");
    match kind {
        Kind::Hmac(hash) => {
            if alg.as_deref().is_some_and(|alg| alg != hash.jwk_hmac_alg()) {
                return Err(dom(
                    "DataError",
                    &format!("The JWK 'alg' member must be \"{}\"", hash.jwk_hmac_alg()),
                ));
            }
        }
        Kind::AesGcm => {
            let expected = format!("A{}GCM", secret.len() * 8);
            if alg.as_deref().is_some_and(|alg| alg != expected) {
                return Err(dom(
                    "DataError",
                    &format!("The JWK 'alg' member must be \"{expected}\""),
                ));
            }
        }
        Kind::Hkdf | Kind::Pbkdf2 => {}
    }
    Ok(secret)
}

fn import_key_impl(
    format: &str,
    key_data: &Value,
    algorithm: &Value,
    extractable: bool,
    usages: &Value,
) -> Outcome {
    let (kind, dict) = import_kind(algorithm, "importKey")?;
    let usages = parse_usages(usages)?;
    check_usages(kind, &usages)?;
    let secret = match (format, kind) {
        ("raw", _) => buffer_source(key_data, "Key data")?,
        ("jwk", Kind::Hmac(_) | Kind::AesGcm) => {
            import_jwk_secret(kind, key_data, extractable, &usages)?
        }
        ("raw" | "jwk" | "spki" | "pkcs8", _) => {
            return Err(not_supported(&format!(
                "Importing {} keys in '{format}' format is not supported",
                kind.name()
            )));
        }
        _ => {
            return Err(type_error(&format!("'{format}' is not a valid KeyFormat")));
        }
    };
    match kind {
        Kind::Hmac(_) => hmac_length_matches(&dict, secret.len())?,
        Kind::AesGcm => aes_key_bytes_supported(secret.len())?,
        Kind::Hkdf | Kind::Pbkdf2 => {
            if extractable {
                return Err(dom(
                    "SyntaxError",
                    &format!("{} keys cannot be extractable", kind.name()),
                ));
            }
        }
    }
    crypto_key_value(KeyRecord {
        kind,
        secret,
        extractable,
        usages,
    })
}

// ── Key export ────────────────────────────────────────────────────────────

fn jwk_value(record: &KeyRecord) -> Value {
    let mut props = HashMap::new();
    props.insert("kty".to_string(), Value::string("oct"));
    props.insert(
        "k".to_string(),
        Value::string(&base64url_encode(&record.secret)),
    );
    let alg = match record.kind {
        Kind::Hmac(hash) => hash.jwk_hmac_alg().to_string(),
        _ => format!("A{}GCM", record.secret.len() * 8),
    };
    props.insert("alg".to_string(), Value::string(&alg));
    props.insert(
        "key_ops".to_string(),
        Value::array(
            record
                .usages
                .iter()
                .map(|usage| Value::string(usage))
                .collect(),
        ),
    );
    props.insert("ext".to_string(), Value::Bool(record.extractable));
    Value::object(props)
}

fn jwk_json(record: &KeyRecord) -> String {
    let alg = match record.kind {
        Kind::Hmac(hash) => hash.jwk_hmac_alg().to_string(),
        _ => format!("A{}GCM", record.secret.len() * 8),
    };
    let ops = record
        .usages
        .iter()
        .map(|usage| format!("\"{usage}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"alg\":\"{alg}\",\"ext\":{},\"k\":\"{}\",\"key_ops\":[{ops}],\"kty\":\"oct\"}}",
        record.extractable,
        base64url_encode(&record.secret)
    )
}

enum Exported {
    Raw(Vec<u8>),
    Jwk(KeyRecord),
}

fn export_key_record(format: &str, key: &Value) -> Result<Exported, Value> {
    let record = key_record(key, "key")?;
    match format {
        "raw" | "jwk" | "spki" | "pkcs8" => {}
        _ => return Err(type_error(&format!("'{format}' is not a valid KeyFormat"))),
    }
    if !record.extractable {
        return Err(dom("InvalidAccessError", "The key is not extractable"));
    }
    match (format, record.kind) {
        ("raw", Kind::Hmac(_) | Kind::AesGcm) => Ok(Exported::Raw(record.secret)),
        ("jwk", Kind::Hmac(_) | Kind::AesGcm) => Ok(Exported::Jwk(record)),
        _ => Err(not_supported(&format!(
            "Exporting {} keys in '{format}' format is not supported",
            record.kind.name()
        ))),
    }
}

fn export_key_impl(format: &str, key: &Value) -> Outcome {
    Ok(match export_key_record(format, key)? {
        Exported::Raw(bytes) => w3cos_core::binary::array_buffer_value(bytes),
        Exported::Jwk(record) => jwk_value(&record),
    })
}

// ── Key generation ────────────────────────────────────────────────────────

fn random_bytes(len: usize) -> Result<Vec<u8>, Value> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|_| {
        dom(
            "OperationError",
            "The system random generator is unavailable",
        )
    })?;
    Ok(bytes)
}

fn aes_length(dict: &Value) -> Result<usize, Value> {
    let length = optional_unsigned(dict, "length")?
        .ok_or_else(|| type_error("AES-GCM parameters require the 'length' member"))?;
    match length {
        128 | 256 => Ok(length as usize / 8),
        192 => Err(not_supported(
            "AES-192 keys are not supported in this runtime",
        )),
        _ => Err(dom(
            "OperationError",
            "AES key length must be 128, 192 or 256 bits",
        )),
    }
}

fn hmac_generate_length(dict: &Value, hash: Hash) -> Result<usize, Value> {
    let length = match optional_unsigned(dict, "length")? {
        None => hash.block_bits(),
        Some(0) => return Err(dom("OperationError", "HMAC key length must not be zero")),
        Some(length) => length,
    };
    if length % 8 != 0 {
        return Err(not_supported(
            "HMAC keys whose length is not a multiple of 8 bits are not supported",
        ));
    }
    Ok(length as usize / 8)
}

fn generate_key_impl(algorithm: &Value, extractable: bool, usages: &Value) -> Outcome {
    let (dict, name) = algorithm_identifier(algorithm)?;
    let kind = match name.as_str() {
        "HMAC" => Kind::Hmac(hash_member(&dict, "HMAC")?),
        "AES-GCM" => Kind::AesGcm,
        _ => return Err(unsupported_algorithm(&name, "generateKey")),
    };
    let usages = parse_usages(usages)?;
    check_usages(kind, &usages)?;
    let len = match kind {
        Kind::Hmac(hash) => hmac_generate_length(&dict, hash)?,
        _ => aes_length(&dict)?,
    };
    crypto_key_value(KeyRecord {
        kind,
        secret: random_bytes(len)?,
        extractable,
        usages,
    })
}

// ── Sign / verify ─────────────────────────────────────────────────────────

fn sign_algorithm_name(algorithm: &Value, operation: &str) -> Result<(), Value> {
    let (_, name) = algorithm_identifier(algorithm)?;
    if name != "HMAC" {
        return Err(unsupported_algorithm(&name, operation));
    }
    Ok(())
}

fn hmac_key(record: &KeyRecord) -> ring::hmac::Key {
    let Kind::Hmac(hash) = record.kind else {
        unreachable!("require_key checked the HMAC algorithm");
    };
    ring::hmac::Key::new(hash.hmac(), &record.secret)
}

fn sign_impl(algorithm: &Value, key: &Value, data: &Value) -> Outcome {
    sign_algorithm_name(algorithm, "sign")?;
    let data = buffer_source(data, "data")?;
    let record = require_key(key, "key", "HMAC", "sign")?;
    let tag = ring::hmac::sign(&hmac_key(&record), &data);
    Ok(w3cos_core::binary::array_buffer_value(
        tag.as_ref().to_vec(),
    ))
}

fn verify_impl(algorithm: &Value, key: &Value, signature: &Value, data: &Value) -> Outcome {
    sign_algorithm_name(algorithm, "verify")?;
    let signature = buffer_source(signature, "signature")?;
    let data = buffer_source(data, "data")?;
    let record = require_key(key, "key", "HMAC", "verify")?;
    Ok(Value::Bool(
        ring::hmac::verify(&hmac_key(&record), &data, &signature).is_ok(),
    ))
}

// ── AES-GCM ───────────────────────────────────────────────────────────────

struct GcmParams {
    iv: Vec<u8>,
    aad: Vec<u8>,
}

fn gcm_params(algorithm: &Value, operation: &str) -> Result<GcmParams, Value> {
    let (dict, name) = algorithm_identifier(algorithm)?;
    if name != "AES-GCM" {
        return Err(unsupported_algorithm(&name, operation));
    }
    let iv = buffer_source(&required_member(&dict, "iv", "AES-GCM")?, "iv")?;
    let additional = dict.get_property("additionalData");
    let aad = if additional.is_undefined() {
        Vec::new()
    } else {
        buffer_source(&additional, "additionalData")?
    };
    match optional_unsigned(&dict, "tagLength")? {
        None | Some(128) => {}
        Some(32 | 64 | 96 | 104 | 112 | 120) => {
            return Err(not_supported(
                "AES-GCM tag lengths other than 128 bits are not supported",
            ));
        }
        Some(_) => return Err(dom("OperationError", "Invalid AES-GCM tag length")),
    }
    if iv.is_empty() {
        return Err(dom("OperationError", "The AES-GCM iv must not be empty"));
    }
    if iv.len() != ring::aead::NONCE_LEN {
        return Err(not_supported(
            "AES-GCM only supports 96-bit (12-byte) iv values in this runtime",
        ));
    }
    Ok(GcmParams { iv, aad })
}

fn gcm_key(record: &KeyRecord) -> Result<ring::aead::LessSafeKey, Value> {
    let algorithm = match record.secret.len() {
        16 => &ring::aead::AES_128_GCM,
        32 => &ring::aead::AES_256_GCM,
        _ => return Err(not_supported("Unsupported AES key length")),
    };
    let key = ring::aead::UnboundKey::new(algorithm, &record.secret)
        .map_err(|_| dom("OperationError", "Invalid AES key"))?;
    Ok(ring::aead::LessSafeKey::new(key))
}

fn gcm_nonce(params: &GcmParams) -> Result<ring::aead::Nonce, Value> {
    ring::aead::Nonce::try_assume_unique_for_key(&params.iv)
        .map_err(|_| dom("OperationError", "Invalid AES-GCM iv"))
}

fn gcm_seal(params: &GcmParams, record: &KeyRecord, mut data: Vec<u8>) -> Result<Vec<u8>, Value> {
    gcm_key(record)?
        .seal_in_place_append_tag(
            gcm_nonce(params)?,
            ring::aead::Aad::from(&params.aad),
            &mut data,
        )
        .map_err(|_| dom("OperationError", "AES-GCM encryption failed"))?;
    Ok(data)
}

fn gcm_open(params: &GcmParams, record: &KeyRecord, mut data: Vec<u8>) -> Result<Vec<u8>, Value> {
    if data.len() < ring::aead::AES_128_GCM.tag_len() {
        return Err(dom(
            "OperationError",
            "The AES-GCM ciphertext is shorter than its tag",
        ));
    }
    let plain_len = gcm_key(record)?
        .open_in_place(
            gcm_nonce(params)?,
            ring::aead::Aad::from(&params.aad),
            &mut data,
        )
        .map_err(|_| dom("OperationError", "AES-GCM decryption failed"))?
        .len();
    data.truncate(plain_len);
    Ok(data)
}

fn encrypt_impl(algorithm: &Value, key: &Value, data: &Value) -> Outcome {
    let params = gcm_params(algorithm, "encrypt")?;
    let data = buffer_source(data, "data")?;
    let record = require_key(key, "key", "AES-GCM", "encrypt")?;
    Ok(w3cos_core::binary::array_buffer_value(gcm_seal(
        &params, &record, data,
    )?))
}

fn decrypt_impl(algorithm: &Value, key: &Value, data: &Value) -> Outcome {
    let params = gcm_params(algorithm, "decrypt")?;
    let data = buffer_source(data, "data")?;
    let record = require_key(key, "key", "AES-GCM", "decrypt")?;
    Ok(w3cos_core::binary::array_buffer_value(gcm_open(
        &params, &record, data,
    )?))
}

// ── Key derivation ────────────────────────────────────────────────────────

struct OkmLen(usize);

impl ring::hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        self.0
    }
}

fn derive_bits_raw(
    algorithm: &Value,
    record: &KeyRecord,
    length: Option<u32>,
) -> Result<Vec<u8>, Value> {
    let Some(length) = length else {
        return Err(dom(
            "OperationError",
            "A derived bit length is required for this algorithm",
        ));
    };
    if length % 8 != 0 {
        return Err(dom(
            "OperationError",
            "The derived bit length must be a multiple of 8",
        ));
    }
    let bytes = length as usize / 8;
    let (dict, _) = algorithm_identifier(algorithm)?;
    match record.kind {
        Kind::Hkdf => {
            let hash = hash_member(&dict, "HKDF")?;
            let salt = buffer_source(&required_member(&dict, "salt", "HKDF")?, "salt")?;
            let info = buffer_source(&required_member(&dict, "info", "HKDF")?, "info")?;
            if bytes == 0 {
                return Ok(Vec::new());
            }
            let prk = ring::hkdf::Salt::new(hash.hkdf(), &salt).extract(&record.secret);
            let info_parts = [info.as_slice()];
            let okm = prk.expand(&info_parts, OkmLen(bytes)).map_err(|_| {
                dom(
                    "OperationError",
                    "HKDF cannot derive more than 255 hash lengths",
                )
            })?;
            let mut out = vec![0u8; bytes];
            okm.fill(&mut out)
                .map_err(|_| dom("OperationError", "HKDF derivation failed"))?;
            Ok(out)
        }
        Kind::Pbkdf2 => {
            let hash = hash_member(&dict, "PBKDF2")?;
            let salt = buffer_source(&required_member(&dict, "salt", "PBKDF2")?, "salt")?;
            required_member(&dict, "iterations", "PBKDF2")?;
            let iterations = optional_unsigned(&dict, "iterations")?.unwrap_or(0);
            let Some(iterations) = NonZeroU32::new(iterations) else {
                return Err(dom("OperationError", "PBKDF2 iterations must not be zero"));
            };
            if bytes == 0 {
                return Err(dom(
                    "OperationError",
                    "The PBKDF2 derived bit length must not be zero",
                ));
            }
            let mut out = vec![0u8; bytes];
            ring::pbkdf2::derive(hash.pbkdf2(), iterations, &salt, &record.secret, &mut out);
            Ok(out)
        }
        _ => unreachable!("derive_record only accepts HKDF and PBKDF2 keys"),
    }
}

fn derive_record(
    algorithm: &Value,
    key: &Value,
    usage: &str,
    operation: &str,
) -> Result<KeyRecord, Value> {
    let (_, name) = algorithm_identifier(algorithm)?;
    let canonical = match name.as_str() {
        "HKDF" => "HKDF",
        "PBKDF2" => "PBKDF2",
        _ => return Err(unsupported_algorithm(&name, operation)),
    };
    require_key(key, "baseKey", canonical, usage)
}

fn derive_bits_impl(algorithm: &Value, key: &Value, length: &Value) -> Outcome {
    let record = derive_record(algorithm, key, "deriveBits", "deriveBits")?;
    let length = if length.is_nullish() {
        None
    } else {
        let number = length.to_number();
        if !number.is_finite() || number < 0.0 || number > u32::MAX as f64 {
            return Err(type_error("length is outside the unsigned long range"));
        }
        Some(number.trunc() as u32)
    };
    Ok(w3cos_core::binary::array_buffer_value(derive_bits_raw(
        algorithm, &record, length,
    )?))
}

fn derive_key_impl(
    algorithm: &Value,
    key: &Value,
    derived: &Value,
    extractable: bool,
    usages: &Value,
) -> Outcome {
    let (kind, dict) = import_kind(derived, "deriveKey")?;
    let bits = match kind {
        Kind::AesGcm => aes_length(&dict)? as u32 * 8,
        Kind::Hmac(hash) => match optional_unsigned(&dict, "length")? {
            None => hash.block_bits(),
            Some(0) => {
                return Err(type_error("HMAC derived key length must not be zero"));
            }
            Some(length) if length % 8 != 0 => {
                return Err(not_supported(
                    "HMAC keys whose length is not a multiple of 8 bits are not supported",
                ));
            }
            Some(length) => length,
        },
        Kind::Hkdf | Kind::Pbkdf2 => {
            return Err(not_supported(&format!(
                "Deriving {} keys is not supported",
                kind.name()
            )));
        }
    };
    let parsed_usages = parse_usages(usages)?;
    check_usages(kind, &parsed_usages)?;
    let record = derive_record(algorithm, key, "deriveKey", "deriveKey")?;
    let secret = derive_bits_raw(algorithm, &record, Some(bits))?;
    crypto_key_value(KeyRecord {
        kind,
        secret,
        extractable,
        usages: parsed_usages,
    })
}

// ── Wrap / unwrap ─────────────────────────────────────────────────────────

fn wrap_key_impl(format: &str, key: &Value, wrapping_key: &Value, algorithm: &Value) -> Outcome {
    let params = gcm_params(algorithm, "wrapKey")?;
    let wrapping = require_key(wrapping_key, "wrappingKey", "AES-GCM", "wrapKey")?;
    let bytes = match export_key_record(format, key)? {
        Exported::Raw(bytes) => bytes,
        Exported::Jwk(record) => jwk_json(&record).into_bytes(),
    };
    Ok(w3cos_core::binary::array_buffer_value(gcm_seal(
        &params, &wrapping, bytes,
    )?))
}

#[allow(clippy::too_many_arguments)]
fn unwrap_key_impl(
    format: &str,
    wrapped: &Value,
    unwrapping_key: &Value,
    unwrap_algorithm: &Value,
    unwrapped_algorithm: &Value,
    extractable: bool,
    usages: &Value,
) -> Outcome {
    let params = gcm_params(unwrap_algorithm, "unwrapKey")?;
    import_kind(unwrapped_algorithm, "unwrapKey")?;
    let wrapped = buffer_source(wrapped, "wrappedKey")?;
    let unwrapping = require_key(unwrapping_key, "unwrappingKey", "AES-GCM", "unwrapKey")?;
    let bytes = gcm_open(&params, &unwrapping, wrapped)?;
    let key_data = match format {
        "raw" => w3cos_core::binary::array_buffer_value(bytes),
        "jwk" => {
            let text = String::from_utf8(bytes)
                .map_err(|_| dom("DataError", "The unwrapped JWK is not UTF-8"))?;
            let parsed =
                w3cos_core::catch_js(|| w3cos_core::json::parse(vec![Value::string(&text)]))
                    .map_err(|_| dom("DataError", "The unwrapped JWK is not valid JSON"))?;
            if !parsed.is_object() {
                return Err(dom("DataError", "The unwrapped JWK is not an object"));
            }
            parsed
        }
        _ => {
            return Err(not_supported(&format!(
                "Unwrapping keys in '{format}' format is not supported"
            )));
        }
    };
    import_key_impl(format, &key_data, unwrapped_algorithm, extractable, usages)
}

// ── Digest ────────────────────────────────────────────────────────────────

fn digest_impl(algorithm: &Value, data: &Value) -> Outcome {
    let (_, name) = algorithm_identifier(algorithm)?;
    let hash = parse_hash_name(&name)
        .ok_or_else(|| not_supported(&format!("Unrecognized digest algorithm: {name}")))?;
    let data = buffer_source(data, "data")?;
    let digest = ring::digest::digest(hash.digest(), &data);
    Ok(w3cos_core::binary::array_buffer_value(
        digest.as_ref().to_vec(),
    ))
}

// ── JavaScript surface ────────────────────────────────────────────────────

fn settle(outcome: Result<Outcome, Value>) -> Value {
    match outcome.and_then(|outcome| outcome) {
        Ok(value) => w3cos_core::promise::resolve(vec![value]),
        Err(error) => w3cos_core::promise::reject(vec![error]),
    }
}

fn method(f: impl Fn(&[Value]) -> Outcome + 'static) -> Value {
    let generation = crate::jsdom::realm_generation();
    crate::jsdom::realm_function(generation, move |_, args| {
        settle(w3cos_core::catch_js_result(|| Ok(f(&args))))
    })
}

fn format_arg(args: &[Value]) -> String {
    arg(args, 0).to_js_string()
}

/// Installs every SubtleCrypto method on a fresh `crypto.subtle` object.
pub fn install_methods(props: &mut HashMap<String, Value>) {
    props.insert(
        "digest".into(),
        method(|args| digest_impl(&arg(args, 0), &arg(args, 1))),
    );
    props.insert(
        "generateKey".into(),
        method(|args| generate_key_impl(&arg(args, 0), arg(args, 1).to_bool(), &arg(args, 2))),
    );
    props.insert(
        "importKey".into(),
        method(|args| {
            import_key_impl(
                &format_arg(args),
                &arg(args, 1),
                &arg(args, 2),
                arg(args, 3).to_bool(),
                &arg(args, 4),
            )
        }),
    );
    props.insert(
        "exportKey".into(),
        method(|args| export_key_impl(&format_arg(args), &arg(args, 1))),
    );
    props.insert(
        "sign".into(),
        method(|args| sign_impl(&arg(args, 0), &arg(args, 1), &arg(args, 2))),
    );
    props.insert(
        "verify".into(),
        method(|args| verify_impl(&arg(args, 0), &arg(args, 1), &arg(args, 2), &arg(args, 3))),
    );
    props.insert(
        "encrypt".into(),
        method(|args| encrypt_impl(&arg(args, 0), &arg(args, 1), &arg(args, 2))),
    );
    props.insert(
        "decrypt".into(),
        method(|args| decrypt_impl(&arg(args, 0), &arg(args, 1), &arg(args, 2))),
    );
    props.insert(
        "deriveBits".into(),
        method(|args| derive_bits_impl(&arg(args, 0), &arg(args, 1), &arg(args, 2))),
    );
    props.insert(
        "deriveKey".into(),
        method(|args| {
            derive_key_impl(
                &arg(args, 0),
                &arg(args, 1),
                &arg(args, 2),
                arg(args, 3).to_bool(),
                &arg(args, 4),
            )
        }),
    );
    props.insert(
        "wrapKey".into(),
        method(|args| {
            wrap_key_impl(
                &format_arg(args),
                &arg(args, 1),
                &arg(args, 2),
                &arg(args, 3),
            )
        }),
    );
    props.insert(
        "unwrapKey".into(),
        method(|args| {
            unwrap_key_impl(
                &format_arg(args),
                &arg(args, 1),
                &arg(args, 2),
                &arg(args, 3),
                &arg(args, 4),
                arg(args, 5).to_bool(),
                &arg(args, 6),
            )
        }),
    );
}

/// `crypto.getRandomValues(typedArray)` per Web Crypto §10.1.1.
pub fn get_random_values(array: &Value) -> Value {
    use w3cos_core::binary;
    let integer_view = binary::is_typed_array(array)
        && !["Float16Array", "Float32Array", "Float64Array"]
            .iter()
            .any(|name| binary::typed_array_instance_of(array, &binary::typed_array_class(name)));
    if !integer_view {
        w3cos_core::throw_value(dom(
            "TypeMismatchError",
            "getRandomValues() requires an integer TypedArray",
        ));
    }
    let Some((_, _, byte_length)) = binary::array_buffer_view_range(array) else {
        w3cos_core::throw_value(dom(
            "TypeMismatchError",
            "getRandomValues() requires an integer TypedArray",
        ));
    };
    if byte_length > 65_536 {
        w3cos_core::throw_value(dom(
            "QuotaExceededError",
            "getRandomValues() cannot generate more than 65536 bytes at once",
        ));
    }
    let mut bytes = vec![0u8; byte_length];
    if getrandom::fill(&mut bytes).is_err() {
        w3cos_core::throw_value(dom(
            "OperationError",
            "The system random generator is unavailable",
        ));
    }
    binary::fill_array_buffer_view(array, &bytes);
    array.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_every_tail_length() {
        for len in 0..8 {
            let bytes: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            let encoded = base64url_encode(&bytes);
            assert!(!encoded.contains('='));
            assert_eq!(base64url_decode(&encoded).unwrap(), bytes);
        }
        assert_eq!(base64url_encode(b"\xfb\xff"), "-_8");
        assert!(base64url_decode("A").is_none());
        assert!(base64url_decode("a+b/").is_none());
    }

    #[test]
    fn jwk_json_is_canonical_and_sorted() {
        let record = KeyRecord {
            kind: Kind::Hmac(Hash::Sha256),
            secret: vec![1, 2, 3],
            extractable: true,
            usages: vec!["sign", "verify"],
        };
        assert_eq!(
            jwk_json(&record),
            r#"{"alg":"HS256","ext":true,"k":"AQID","key_ops":["sign","verify"],"kty":"oct"}"#
        );
    }
}
