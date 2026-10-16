//! Decentralized transport for multi-device handoff.
//!
//! Devices speak libp2p over QUIC. Payloads are sealed before they are handed
//! to the swarm, so a circuit relay only forwards ciphertext. Addresses are
//! republished as signed presence records; when both sides move, the next
//! dial is resolved from Kademlia and, if that misses, from a Nostr note.

mod config;
mod identity;
mod node;
mod nostr;
mod presence;
mod reconnect;
mod stun;
mod wallet;

pub mod hpke;

pub use config::NetConfig;
pub use identity::{
    DeviceIdentity, did_key_ed25519, ed25519_public_from_did, fingerprint, verify_ed25519,
};
pub use node::Endpoint;
pub use nostr::{Kind as NostrKind, fetch_latest, publish_presence};
pub use presence::{Presence, SignedPresence};
pub use reconnect::{ResolveStep, plan_reconnect, prefer_paths};
pub use stun::{binding_request, parse_binding_response};
pub use wallet::LocalWallet;

/// How a paired device is reachable right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkMode {
    /// QUIC path, including a connection that migrated to a new address.
    Direct,
    /// Circuit Relay v2. The bytes on this path are still sealed.
    Relay,
    /// No live path.
    Offline,
}

impl LinkMode {
    pub fn message_id(self) -> &'static str {
        match self {
            Self::Direct => "link-direct",
            Self::Relay => "link-relay",
            Self::Offline => "link-offline",
        }
    }

    pub fn from_addr(addr: &str) -> Self {
        if addr.contains("/p2p-circuit") {
            Self::Relay
        } else {
            Self::Direct
        }
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

pub(crate) fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let hi = hex_nibble(bytes[index])?;
        let lo = hex_nibble(bytes[index + 1])?;
        out.push((hi << 4) | lo);
        index += 2;
    }
    Some(out)
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

    #[test]
    fn vendored_quic_enables_connection_migration() {
        assert!(libp2p_quic::CONNECTION_MIGRATION_ENABLED);
        let source = include_str!("../../../third_party/libp2p-quic/src/config.rs");
        assert!(
            source.contains("server_config.migration(true)"),
            "QUIC migration must stay enabled in the vendored transport"
        );
        assert!(
            !source.contains("server_config.migration(false)"),
            "stock libp2p-quic disables migration; that line must not return"
        );
        let connection = include_str!("../../../third_party/libp2p-quic/src/connection.rs");
        assert!(
            connection.contains("StreamMuxerEvent::AddressChange"),
            "a migrated remote address has to be reported to the swarm"
        );
    }

    #[test]
    fn relay_addresses_are_not_classified_as_direct() {
        assert_eq!(
            LinkMode::from_addr("/ip4/127.0.0.1/udp/1/quic-v1"),
            LinkMode::Direct
        );
        assert_eq!(
            LinkMode::from_addr("/ip4/127.0.0.1/udp/1/quic-v1/p2p/12D3KooW/p2p-circuit"),
            LinkMode::Relay
        );
        assert_eq!(LinkMode::Offline.message_id(), "link-offline");
        assert_eq!(LinkMode::Direct.message_id(), "link-direct");
        assert_eq!(LinkMode::Relay.message_id(), "link-relay");
    }
}
