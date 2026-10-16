//! A signed address card. The signature is the device's Ed25519 key, which
//! was exchanged at pairing. Relays and the DHT store the card, not the
//! handoff payload.

use libp2p::identity::{self, ed25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{hex_decode, hex_encode};

pub const PREFIX: &str = "W3PRES1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub v: u8,
    pub name: String,
    pub fp: String,
    pub x25519: String,
    pub ed25519: String,
    pub peer: String,
    pub addrs: Vec<String>,
    pub ts: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedPresence {
    pub presence: Presence,
    pub signature: Vec<u8>,
    pub body: Vec<u8>,
}

impl Presence {
    pub fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

pub fn sign(keypair: &identity::Keypair, presence: &Presence) -> Option<SignedPresence> {
    let body = presence.canonical();
    let signature = keypair.sign(&body).ok()?;
    Some(SignedPresence {
        presence: presence.clone(),
        signature,
        body,
    })
}

pub fn verify(ed25519_public: &[u8; 32], signed: &SignedPresence) -> bool {
    let Ok(public) = ed25519::PublicKey::try_from_bytes(ed25519_public) else {
        return false;
    };
    if !public.verify(&signed.body, &signed.signature) {
        return false;
    }
    let Ok(parsed) = serde_json::from_slice::<Presence>(&signed.body) else {
        return false;
    };
    parsed == signed.presence && parsed.ed25519 == hex_encode(ed25519_public)
}

pub fn encode(signed: &SignedPresence) -> Vec<u8> {
    let mut out = PREFIX.as_bytes().to_vec();
    out.push(b'\n');
    out.extend_from_slice(hex_encode(&signed.signature).as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&signed.body);
    out
}

pub fn decode(bytes: &[u8]) -> Option<SignedPresence> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.splitn(3, '\n');
    if lines.next()? != PREFIX {
        return None;
    }
    let signature = hex_decode(lines.next()?)?;
    let body = lines.next()?.as_bytes().to_vec();
    let presence = serde_json::from_slice(&body).ok()?;
    Some(SignedPresence {
        presence,
        signature,
        body,
    })
}

pub fn record_key(fingerprint: &str) -> Vec<u8> {
    Sha256::new()
        .chain_update(b"w3cos/presence/v1\n")
        .chain_update(fingerprint.as_bytes())
        .finalize()
        .to_vec()
}

/// Presence is an address card. A handoff secret must not be placed in it.
pub fn contains_plaintext(signed: &SignedPresence, secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    signed
        .body
        .windows(secret.len())
        .any(|window| window == secret.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;

    #[test]
    fn a_tampered_card_does_not_verify_and_payloads_are_absent() {
        let device = DeviceIdentity::from_seed([4; 32]);
        let presence = Presence {
            v: 1,
            name: "书桌".into(),
            fp: device.fingerprint(),
            x25519: hex_encode(&device.x25519_public),
            ed25519: hex_encode(&device.ed25519_public),
            peer: device.peer_id.to_string(),
            addrs: vec!["/ip4/203.0.113.4/udp/4001/quic-v1".into()],
            ts: 10,
        };
        let signed = sign(&device.keypair, &presence).expect("sign");
        assert!(verify(&device.ed25519_public, &signed));
        assert!(!contains_plaintext(&signed, "draft-secret"));
        let encoded = encode(&signed);
        let decoded = decode(&encoded).expect("decode");
        assert_eq!(decoded.presence.addrs, presence.addrs);
        let mut tampered = signed.clone();
        tampered.body.push(b'x');
        tampered.presence.name.push('x');
        assert!(!verify(&device.ed25519_public, &tampered));
        let other = DeviceIdentity::from_seed([5; 32]);
        assert!(!verify(&other.ed25519_public, &signed));
    }
}
