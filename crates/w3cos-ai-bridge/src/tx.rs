//! One unsigned legacy transaction, decoded for a person.
//!
//! The model proposes fields. It cannot hand over a signed payload. Broadcast
//! builds `eth_sendRawTransaction` only from bytes this process just signed.

use serde_json::{Map, Value, json};

use crate::copy;

const ALLOWED: &[&str] = &[
    "chain_id",
    "data",
    "gas_limit",
    "gas_price",
    "nonce",
    "to",
    "value",
];

/// Fields a person can read before anything is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsignedTx {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_price: u128,
    pub gas_limit: u64,
    pub to: [u8; 20],
    pub to_hex: String,
    pub value: [u8; 32],
    pub value_dec: String,
    pub gas_price_dec: String,
    pub data: Vec<u8>,
    pub data_hex: String,
}

/// Parse a proposal object. Extra keys, including a raw payload, fail.
pub fn parse_unsigned(value: &Value) -> Result<UnsignedTx, &'static str> {
    let object = value.as_object().ok_or("fields")?;
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err("raw");
    }
    let chain_id = json_u64(object.get("chain_id").ok_or("fields")?).ok_or("fields")?;
    if chain_id == 0 {
        return Err("fields");
    }
    let nonce = json_u64(object.get("nonce").ok_or("fields")?).ok_or("fields")?;
    let (gas_price, gas_price_dec) =
        json_u128(object.get("gas_price").ok_or("fields")?).ok_or("fields")?;
    let gas_limit = json_u64(object.get("gas_limit").ok_or("fields")?).ok_or("fields")?;
    if !(21_000..=30_000_000).contains(&gas_limit) {
        return Err("fields");
    }
    let to_hex = object
        .get("to")
        .and_then(|item| item.as_str())
        .ok_or("fields")?;
    let to = parse_address(to_hex).ok_or("fields")?;
    let (value_bytes, value_dec) =
        json_be32(object.get("value").ok_or("fields")?).ok_or("fields")?;
    let data_hex = match object.get("data") {
        None => "0x".to_string(),
        Some(item) => item.as_str().ok_or("fields")?.trim().to_string(),
    };
    let data = parse_data(&data_hex).ok_or("fields")?;
    let data_hex = if data.is_empty() {
        "0x".to_string()
    } else {
        format!("0x{}", hex_encode(&data))
    };
    Ok(UnsignedTx {
        chain_id,
        nonce,
        gas_price,
        gas_limit,
        to,
        to_hex: format!("0x{}", hex_encode(&to)),
        value: value_bytes,
        value_dec,
        gas_price_dec,
        data,
        data_hex,
    })
}

pub fn parse_stored(text: &str) -> Result<UnsignedTx, &'static str> {
    let value: Value = serde_json::from_str(text).map_err(|_| "fields")?;
    parse_unsigned(&value)
}

/// Stable JSON. No signature and no signed payload.
pub fn canonical(tx: &UnsignedTx) -> String {
    let mut map = Map::new();
    map.insert("chain_id".into(), json!(tx.chain_id));
    map.insert("data".into(), json!(tx.data_hex));
    map.insert("gas_limit".into(), json!(tx.gas_limit));
    map.insert("gas_price".into(), json!(tx.gas_price_dec));
    map.insert("nonce".into(), json!(tx.nonce));
    map.insert("to".into(), json!(tx.to_hex));
    map.insert("value".into(), json!(tx.value_dec));
    Value::Object(map).to_string()
}

/// One fluent line per field, for the sign sheet and the broadcast sheet.
pub fn preview_lines(locale: &str, tx: &UnsignedTx) -> String {
    let data = if tx.data.is_empty() {
        copy::line(locale, "tx-data-empty", &[])
    } else if tx.data_hex.chars().count() > 18 {
        let head: String = tx.data_hex.chars().take(18).collect();
        format!("{head}…")
    } else {
        tx.data_hex.clone()
    };
    [
        copy::line(locale, "tx-field-to", &[("value", tx.to_hex.as_str())]),
        copy::line(
            locale,
            "tx-field-value",
            &[("value", tx.value_dec.as_str())],
        ),
        copy::line(
            locale,
            "tx-field-nonce",
            &[("value", &tx.nonce.to_string())],
        ),
        copy::line(
            locale,
            "tx-field-gas-price",
            &[("value", tx.gas_price_dec.as_str())],
        ),
        copy::line(
            locale,
            "tx-field-gas",
            &[("value", &tx.gas_limit.to_string())],
        ),
        copy::line(
            locale,
            "tx-field-chain",
            &[("value", &tx.chain_id.to_string())],
        ),
        copy::line(locale, "tx-field-data", &[("value", data.as_str())]),
    ]
    .join("\n")
}

pub fn preview_from_json(locale: &str, raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let tx = parse_unsigned(&value).ok()?;
    Some(preview_lines(locale, &tx))
}

/// JSON-RPC body for the signed bytes this process is holding.
pub fn send_body(raw: &str) -> Result<String, &'static str> {
    if !is_signed_raw(raw) {
        return Err("raw");
    }
    Ok(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendRawTransaction",
        "params": [raw]
    })
    .to_string())
}

/// Transaction hash from a send response.
pub fn interpret_send(body: &str) -> Result<String, &'static str> {
    if body.len() > 64 * 1024 {
        return Err("large");
    }
    let value: Value = serde_json::from_str(body).map_err(|_| "json")?;
    if value.get("error").is_some() {
        return Err("rpc");
    }
    let hash = value
        .get("result")
        .and_then(|item| item.as_str())
        .ok_or("json")?;
    if !crate::chain::is_tx_hash(hash) {
        return Err("json");
    }
    Ok(hash.to_string())
}

fn is_signed_raw(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("0x") else {
        return false;
    };
    (64..=16_384).contains(&rest.len())
        && rest.len().is_multiple_of(2)
        && rest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn json_u64(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    let text = value.as_str()?.trim();
    if !decimal_ok(text) {
        return None;
    }
    text.parse().ok()
}

fn json_u128(value: &Value) -> Option<(u128, String)> {
    let text = match value.as_u64() {
        Some(number) => number.to_string(),
        None => value.as_str()?.trim().to_string(),
    };
    let number = decimal_to_u128(&text)?;
    Some((number, text))
}

fn json_be32(value: &Value) -> Option<([u8; 32], String)> {
    let text = match value.as_u64() {
        Some(number) => number.to_string(),
        None => value.as_str()?.trim().to_string(),
    };
    let bytes = decimal_to_be32(&text)?;
    Some((bytes, text))
}

fn decimal_ok(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 78
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
}

fn decimal_to_be32(text: &str) -> Option<[u8; 32]> {
    if !decimal_ok(text) {
        return None;
    }
    let mut bytes = [0u8; 32];
    for digit in text.bytes() {
        let mut carry = (digit - b'0') as u16;
        for byte in bytes.iter_mut().rev() {
            let next = u16::from(*byte) * 10 + carry;
            *byte = (next & 0xff) as u8;
            carry = next >> 8;
        }
        if carry != 0 {
            return None;
        }
    }
    Some(bytes)
}

fn decimal_to_u128(text: &str) -> Option<u128> {
    let bytes = decimal_to_be32(text)?;
    if bytes[..16].iter().any(|byte| *byte != 0) {
        return None;
    }
    let mut narrow = [0u8; 16];
    narrow.copy_from_slice(&bytes[16..]);
    Some(u128::from_be_bytes(narrow))
}

fn parse_address(value: &str) -> Option<[u8; 20]> {
    let rest = value.trim().strip_prefix("0x")?;
    if rest.len() != 40 || !rest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 20];
    for (index, byte) in out.iter_mut().enumerate() {
        let hi = hex_nibble(rest.as_bytes()[index * 2])?;
        let lo = hex_nibble(rest.as_bytes()[index * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

fn parse_data(value: &str) -> Option<Vec<u8>> {
    let rest = value.strip_prefix("0x")?;
    if rest.len() > 512 || !rest.len().is_multiple_of(2) {
        return None;
    }
    if !rest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = Vec::with_capacity(rest.len() / 2);
    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let hi = hex_nibble(bytes[index])?;
        let lo = hex_nibble(bytes[index + 1])?;
        out.push((hi << 4) | lo);
        index += 2;
    }
    Some(out)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "chain_id": 1,
            "nonce": "7",
            "gas_price": "1000000000",
            "gas_limit": 21000,
            "to": "0x1111111111111111111111111111111111111111",
            "value": "1",
            "data": "0x"
        })
    }

    #[test]
    fn fields_decode_and_a_raw_payload_is_refused() {
        let tx = parse_unsigned(&sample()).unwrap();
        assert_eq!(tx.chain_id, 1);
        assert_eq!(tx.nonce, 7);
        assert_eq!(tx.gas_price, 1_000_000_000);
        assert_eq!(tx.value_dec, "1");
        assert!(tx.data.is_empty());
        let again = parse_stored(&canonical(&tx)).unwrap();
        assert_eq!(again, tx);
        assert!(!canonical(&tx).contains("eth_send"));
        let mut smuggled = sample();
        smuggled
            .as_object_mut()
            .unwrap()
            .insert("raw".into(), json!("0xabcd"));
        assert_eq!(parse_unsigned(&smuggled).unwrap_err(), "raw");
        assert!(parse_unsigned(&json!({"to": "0x11"})).is_err());
        assert!(
            parse_unsigned(&json!({
                "chain_id": 0,
                "nonce": 1,
                "gas_price": "1",
                "gas_limit": 21000,
                "to": "0x1111111111111111111111111111111111111111",
                "value": "0"
            }))
            .is_err()
        );
        let lines = preview_lines("zh-CN", &tx);
        assert!(lines.contains("1111"));
        assert!(lines.contains('1'));
        assert!(!lines.contains("eth_send"));
    }

    #[test]
    fn send_uses_only_the_raw_this_process_holds() {
        let raw = format!("0x{}", "ab".repeat(80));
        let body = send_body(&raw).unwrap();
        assert!(body.contains("eth_sendRawTransaction"));
        assert!(body.contains(&raw));
        assert!(send_body("0xzz").is_err());
        assert!(send_body("0xabcd").is_err());
        let hash = format!("0x{}", "cd".repeat(32));
        let response = format!(r#"{{"jsonrpc":"2.0","id":1,"result":"{hash}"}}"#);
        assert_eq!(interpret_send(&response).unwrap(), hash);
        assert!(interpret_send(r#"{"jsonrpc":"2.0","id":1,"error":{"message":"no"}}"#).is_err());
    }
}
