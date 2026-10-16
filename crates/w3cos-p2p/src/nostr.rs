//! A tiny NIP-01 client for presence only.
//!
//! The event content is the signed address card. Handoff ciphertext is never
//! written here. Kind 30078 is a replaceable address book entry keyed by the
//! device fingerprint, so the latest addresses win.

use std::net::TcpStream;
use std::time::Duration;

use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use tungstenite::{Message, connect, stream::MaybeTlsStream};

use crate::hex_encode;
use crate::identity::nostr_secret;
use crate::presence::SignedPresence;

pub const KIND: u32 = 30078;

pub type Kind = u32;

pub fn publish_presence(
    seed: &[u8; 32],
    fingerprint: &str,
    content: &str,
    relays: &[String],
    timeout: Duration,
) -> bool {
    let Some(event) = build_event(seed, fingerprint, content, now_unix()) else {
        return false;
    };
    let text = serde_json::to_string(&event).unwrap_or_default();
    let frame = format!(r#"["EVENT",{text}]"#);
    for relay in relays {
        if send_and_wait_ok(relay, &frame, &event_id(&event), timeout) {
            return true;
        }
    }
    false
}

pub fn fetch_latest(
    fingerprint: &str,
    relays: &[String],
    timeout: Duration,
) -> Option<SignedPresence> {
    let filter = serde_json::json!({
        "kinds": [KIND],
        "#d": [fingerprint],
        "limit": 1
    });
    let req = format!(r#"["REQ","w3",{filter}]"#);
    for relay in relays {
        if let Some(content) = request_content(relay, &req, timeout) {
            if let Ok(signed) = decode_content(&content) {
                return Some(signed);
            }
        }
    }
    None
}

fn decode_content(content: &str) -> Result<SignedPresence, ()> {
    crate::presence::decode(content.as_bytes()).ok_or(())
}

fn build_event(
    seed: &[u8; 32],
    fingerprint: &str,
    content: &str,
    created_at: u64,
) -> Option<serde_json::Value> {
    let secret = nostr_secret(seed);
    let signing = SigningKey::from_bytes(&secret).ok()?;
    let verifying = signing.verifying_key();
    let pubkey = hex_encode(verifying.to_bytes().as_slice());
    let tags = serde_json::json!([["d", fingerprint]]);
    let preimage = serde_json::json!([0, pubkey, created_at, KIND, tags, content]);
    let serialized = serde_json::to_vec(&preimage).ok()?;
    let id_bytes = Sha256::digest(&serialized);
    let mut aux = [0u8; 32];
    let _ = getrandom::getrandom(&mut aux);
    let signature: Signature = signing.sign_raw(&id_bytes, &aux).ok()?;
    let id = hex_encode(&id_bytes);
    let sig = hex_encode(signature.to_bytes().as_slice());
    Some(serde_json::json!({
        "id": id,
        "pubkey": pubkey,
        "created_at": created_at,
        "kind": KIND,
        "tags": tags,
        "content": content,
        "sig": sig,
    }))
}

fn event_id(event: &serde_json::Value) -> String {
    event
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

pub fn verify_event(event: &serde_json::Value) -> bool {
    let Some(pubkey) = event.get("pubkey").and_then(|value| value.as_str()) else {
        return false;
    };
    let Some(id) = event.get("id").and_then(|value| value.as_str()) else {
        return false;
    };
    let Some(sig) = event.get("sig").and_then(|value| value.as_str()) else {
        return false;
    };
    let Some(created_at) = event.get("created_at").and_then(|value| value.as_u64()) else {
        return false;
    };
    let Some(kind) = event.get("kind").and_then(|value| value.as_u64()) else {
        return false;
    };
    let tags = event.get("tags").cloned().unwrap_or(serde_json::json!([]));
    let content = event
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let preimage = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    let Ok(serialized) = serde_json::to_vec(&preimage) else {
        return false;
    };
    let digest = Sha256::digest(&serialized);
    if hex_encode(&digest) != id {
        return false;
    }
    let Ok(pk) = hex_to_32(pubkey) else {
        return false;
    };
    let Ok(sig_bytes) = hex_to_64(sig) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    let Ok(signature) = Signature::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    verifying.verify_raw(&digest, &signature).is_ok()
}

fn hex_to_32(text: &str) -> Result<[u8; 32], ()> {
    let bytes = crate::hex_decode(text).ok_or(())?;
    bytes.try_into().map_err(|_| ())
}

fn hex_to_64(text: &str) -> Result<[u8; 64], ()> {
    let bytes = crate::hex_decode(text).ok_or(())?;
    bytes.try_into().map_err(|_| ())
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn send_and_wait_ok(relay: &str, frame: &str, id: &str, timeout: Duration) -> bool {
    let Ok(mut socket) = dial_relay(relay, timeout) else {
        return false;
    };
    if socket.send(Message::Text(frame.into())).is_err() {
        return false;
    }
    for _ in 0..4 {
        let Ok(message) = socket.read() else {
            return false;
        };
        let Message::Text(text) = message else {
            continue;
        };
        if text.contains(id) && text.contains("OK") {
            return true;
        }
    }
    false
}

fn request_content(relay: &str, req: &str, timeout: Duration) -> Option<String> {
    let mut socket = dial_relay(relay, timeout).ok()?;
    socket.send(Message::Text(req.into())).ok()?;
    for _ in 0..6 {
        let message = socket.read().ok()?;
        let Message::Text(text) = message else {
            continue;
        };
        if text.contains("EOSE") {
            return None;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(items) = value.as_array() else {
            continue;
        };
        if items.first().and_then(|item| item.as_str()) != Some("EVENT") {
            continue;
        }
        let event = items.get(2)?;
        if !verify_event(event) {
            continue;
        }
        return event
            .get("content")
            .and_then(|item| item.as_str())
            .map(str::to_string);
    }
    None
}

fn dial_relay(
    relay: &str,
    timeout: Duration,
) -> Result<tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, tungstenite::Error> {
    let (socket, _) = connect(relay)?;
    arm_timeout(&socket, timeout);
    Ok(socket)
}

fn arm_timeout(socket: &tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, timeout: Duration) {
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_ref(),
        _ => return,
    };
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::presence::{Presence, encode, sign};
    use std::net::TcpListener;
    use tungstenite::accept;

    #[test]
    fn events_are_signed_and_do_not_carry_handoff_plaintext() {
        let device = DeviceIdentity::from_seed([11; 32]);
        let presence = Presence {
            v: 1,
            name: "手机".into(),
            fp: device.fingerprint(),
            x25519: hex_encode(&device.x25519_public),
            ed25519: hex_encode(&device.ed25519_public),
            peer: device.peer_id.to_string(),
            addrs: vec!["/ip4/198.51.100.10/udp/9/quic-v1".into()],
            ts: 20,
        };
        let signed = sign(&device.keypair, &presence).unwrap();
        let content = String::from_utf8(encode(&signed)).unwrap();
        assert!(!content.contains("draft-secret-token"));
        let event = build_event(&[11; 32], &device.fingerprint(), &content, 30).unwrap();
        assert!(verify_event(&event));
        let dumped = event.to_string();
        assert!(!dumped.contains("draft-secret-token"));
        let mut broken = event.clone();
        broken["sig"] = serde_json::json!("00".repeat(64));
        assert!(!verify_event(&broken));
    }

    #[test]
    fn a_local_relay_exchanges_only_the_address_card() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let device = DeviceIdentity::from_seed([12; 32]);
        let presence = Presence {
            v: 1,
            name: "书桌".into(),
            fp: device.fingerprint(),
            x25519: hex_encode(&device.x25519_public),
            ed25519: hex_encode(&device.ed25519_public),
            peer: device.peer_id.to_string(),
            addrs: vec!["/ip4/203.0.113.20/udp/4001/quic-v1".into()],
            ts: 40,
        };
        let signed = sign(&device.keypair, &presence).unwrap();
        let content = String::from_utf8(encode(&signed)).unwrap();
        let fingerprint = device.fingerprint();
        let content_for_relay = content.clone();
        let handle = std::thread::spawn(move || mini_relay(listener, &content_for_relay));
        let relays = vec![format!("ws://127.0.0.1:{port}")];
        assert!(publish_presence(
            &[12; 32],
            &fingerprint,
            &content,
            &relays,
            Duration::from_secs(3),
        ));
        let fetched = fetch_latest(&fingerprint, &relays, Duration::from_secs(3)).expect("card");
        assert_eq!(fetched.presence.addrs, presence.addrs);
        assert!(crate::presence::verify(&device.ed25519_public, &fetched));
        let seen = handle.join().unwrap();
        assert!(
            !seen.contains("draft-secret-token"),
            "relay saw a payload: {seen}"
        );
        assert!(seen.contains("203.0.113.20"));
    }

    fn mini_relay(listener: TcpListener, stored_content: &str) -> String {
        let _ = stored_content;
        let mut seen = String::new();
        let mut stored: Option<serde_json::Value> = None;
        // One connection publishes, the next one fetches.
        for _ in 0..2 {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
            let Ok(mut socket) = accept(stream) else {
                continue;
            };
            for _ in 0..6 {
                let Ok(message) = socket.read() else {
                    break;
                };
                let Message::Text(text) = message else {
                    continue;
                };
                seen.push_str(&text);
                seen.push('\n');
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                let Some(items) = value.as_array() else {
                    continue;
                };
                match items.first().and_then(|item| item.as_str()) {
                    Some("EVENT") => {
                        let event = items.get(1).cloned().unwrap_or(serde_json::json!({}));
                        let id = event
                            .get("id")
                            .and_then(|item| item.as_str())
                            .unwrap_or("")
                            .to_string();
                        stored = Some(event);
                        let ok = serde_json::json!(["OK", id, true, ""]);
                        let _ = socket.send(Message::Text(ok.to_string().into()));
                    }
                    Some("REQ") => {
                        let sub = items.get(1).and_then(|item| item.as_str()).unwrap_or("w3");
                        if let Some(event) = &stored {
                            let frame = serde_json::json!(["EVENT", sub, event]);
                            let _ = socket.send(Message::Text(frame.to_string().into()));
                        }
                        let eose = serde_json::json!(["EOSE", sub]);
                        let _ = socket.send(Message::Text(eose.to_string().into()));
                        break;
                    }
                    _ => {}
                }
            }
            let _ = socket.close(None);
        }
        seen
    }
}
