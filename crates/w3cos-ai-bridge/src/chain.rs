//! Read-only chain calls against an endpoint the person typed.
//!
//! There is no built-in endpoint. Balance, nonce, and transaction status are
//! the only methods. Nothing here signs or broadcasts.

use std::io::Read;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::Agent;

/// What a confirmed read is allowed to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainRead {
    Balance,
    Nonce,
    TxStatus,
}

/// A usable http(s) endpoint. `origin` has no path and no query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainEndpoint {
    pub url: String,
    pub origin: String,
}

/// `http://` or `https://` with a host. Empty, other schemes, and userinfo fail.
pub fn parse_endpoint(raw: &str) -> Option<ChainEndpoint> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 512 || raw.chars().any(char::is_whitespace) {
        return None;
    }
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = raw.strip_prefix("http://") {
        ("http", rest)
    } else {
        return None;
    };
    if rest.is_empty() || rest.contains('@') {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.starts_with(':') || host.ends_with(':') {
        return None;
    }
    Some(ChainEndpoint {
        url: raw.to_string(),
        origin: format!("{scheme}://{host}"),
    })
}

/// JSON-RPC body for one read. The address is the local wallet. The hash is
/// only used for [`ChainRead::TxStatus`].
pub fn request_body(kind: ChainRead, address: &str, tx_hash: &str) -> Result<String, &'static str> {
    let (method, params) = match kind {
        ChainRead::Balance => ("eth_getBalance", json!([address, "latest"])),
        ChainRead::Nonce => ("eth_getTransactionCount", json!([address, "latest"])),
        ChainRead::TxStatus => {
            if !is_tx_hash(tx_hash) {
                return Err("bad-hash");
            }
            ("eth_getTransactionReceipt", json!([tx_hash]))
        }
    };
    Ok(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    })
    .to_string())
}

/// Turn a JSON-RPC body into a short fact. `Ok(None)` means the transaction
/// is not on chain yet.
pub fn interpret(kind: ChainRead, body: &str) -> Result<Option<String>, &'static str> {
    if body.len() > 64 * 1024 {
        return Err("large");
    }
    let value: Value = serde_json::from_str(body).map_err(|_| "json")?;
    if value.get("error").is_some() {
        return Err("rpc");
    }
    let result = value.get("result").ok_or("json")?;
    match kind {
        ChainRead::Balance | ChainRead::Nonce => {
            let hex = result.as_str().ok_or("json")?;
            hex_quantity(hex).map(Some)
        }
        ChainRead::TxStatus => {
            if result.is_null() {
                return Ok(None);
            }
            let status = result
                .get("status")
                .and_then(|item| item.as_str())
                .ok_or("json")?;
            match status {
                "0x1" | "0x01" => Ok(Some("ok".into())),
                "0x0" | "0x00" => Ok(Some("fail".into())),
                _ => Err("json"),
            }
        }
    }
}

/// POST `body` to `url` and return the response body. Redirects are not followed.
pub fn post_json(url: &str, body: &str) -> Result<String, String> {
    let Some(endpoint) = parse_endpoint(url) else {
        return Err("endpoint".into());
    };
    let agent = Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .max_redirects(0)
        .build()
        .new_agent();
    let mut response = agent
        .post(&endpoint.url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send(body.as_bytes())
        .map_err(|err| err.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("http {}", response.status()));
    }
    let reader = response.body_mut().as_reader();
    let mut bytes = Vec::new();
    reader
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    String::from_utf8(bytes).map_err(|_| "utf8".into())
}

pub fn is_tx_hash(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("0x") else {
        return false;
    };
    rest.len() == 64 && rest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_quantity(value: &str) -> Result<String, &'static str> {
    let Some(rest) = value.strip_prefix("0x") else {
        return Err("json");
    };
    if rest.is_empty() || rest.len() > 32 || !rest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("json");
    }
    let digits = rest.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let number = u128::from_str_radix(digits, 16).map_err(|_| "json")?;
    Ok(number.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn only_a_typed_http_endpoint_is_accepted() {
        let source = include_str!("chain.rs").to_ascii_lowercase();
        for word in [
            String::from_iter(['i', 'n', 'f', 'u', 'r', 'a']),
            String::from_iter(['a', 'l', 'c', 'h', 'e', 'm', 'y']),
            String::from_iter(['c', 'l', 'o', 'u', 'd', 'f', 'l', 'a', 'r', 'e']),
        ] {
            assert!(!source.contains(&word), "{word}");
        }
        assert!(parse_endpoint("").is_none());
        assert!(parse_endpoint("https://").is_none());
        assert!(parse_endpoint("file:///tmp/rpc").is_none());
        assert!(parse_endpoint("http://user:pass@127.0.0.1:8545").is_none());
        let endpoint = parse_endpoint("http://127.0.0.1:8545/rpc?token=secret").unwrap();
        assert_eq!(endpoint.origin, "http://127.0.0.1:8545");
        assert!(!endpoint.origin.contains("secret"));
    }

    #[test]
    fn reads_parse_without_signing_or_broadcasting() {
        let balance = request_body(ChainRead::Balance, "0xabc", "").unwrap();
        assert!(balance.contains("eth_getBalance"));
        assert!(!balance.contains("eth_send"));
        assert_eq!(
            interpret(
                ChainRead::Balance,
                r#"{"jsonrpc":"2.0","id":1,"result":"0x10"}"#
            )
            .unwrap(),
            Some("16".into())
        );
        assert_eq!(
            interpret(
                ChainRead::Nonce,
                r#"{"jsonrpc":"2.0","id":1,"result":"0x0"}"#
            )
            .unwrap(),
            Some("0".into())
        );
        assert_eq!(
            interpret(
                ChainRead::TxStatus,
                r#"{"jsonrpc":"2.0","id":1,"result":null}"#
            )
            .unwrap(),
            None
        );
        assert_eq!(
            interpret(
                ChainRead::TxStatus,
                r#"{"jsonrpc":"2.0","id":1,"result":{"status":"0x1"}}"#
            )
            .unwrap(),
            Some("ok".into())
        );
        assert!(
            interpret(
                ChainRead::Balance,
                r#"{"jsonrpc":"2.0","id":1,"error":{"message":"no"}}"#
            )
            .is_err()
        );
        assert!(request_body(ChainRead::TxStatus, "0xabc", "0x11").is_err());
        let hash = format!("0x{}", "ab".repeat(32));
        assert!(request_body(ChainRead::TxStatus, "0xabc", &hash).is_ok());
    }

    #[test]
    fn post_json_talks_only_to_the_given_url() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = socket.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            assert!(head.starts_with("POST "), "{head}");
            let payload = r#"{"jsonrpc":"2.0","id":1,"result":"0x2"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = socket.write_all(response.as_bytes());
        });
        let url = format!("http://127.0.0.1:{port}");
        let body = request_body(ChainRead::Nonce, "0xabc", "").unwrap();
        let response = post_json(&url, &body).unwrap();
        assert_eq!(
            interpret(ChainRead::Nonce, &response).unwrap(),
            Some("2".into())
        );
        assert!(post_json("notaurl", &body).is_err());
    }
}
