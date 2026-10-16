//! Default peer lists are starting points. A person can add community or
//! self-hosted nodes; nothing here is a single required provider.

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetConfig {
    pub bootstrap: Vec<String>,
    pub relays: Vec<String>,
    pub stun: Vec<String>,
    pub nostr: Vec<String>,
    pub enable_mdns: bool,
    pub enable_dht: bool,
    pub enable_nostr: bool,
    pub enable_relay_server: bool,
    pub enable_dcutr: bool,
    pub dial_public: bool,
    pub listen: Vec<String>,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ExtraNodes {
    #[serde(default)]
    pub bootstrap: Vec<String>,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default)]
    pub stun: Vec<String>,
    #[serde(default)]
    pub nostr: Vec<String>,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self::production()
    }
}

impl NetConfig {
    /// Public defaults. Every list has more than one operator.
    pub fn production() -> Self {
        Self {
            bootstrap: default_bootstrap(),
            relays: default_relays(),
            stun: default_stun(),
            nostr: default_nostr(),
            enable_mdns: true,
            enable_dht: true,
            enable_nostr: true,
            enable_relay_server: true,
            enable_dcutr: true,
            dial_public: true,
            listen: vec![
                "/ip4/0.0.0.0/udp/0/quic-v1".to_string(),
                "/ip6/::/udp/0/quic-v1".to_string(),
            ],
            name: std::env::var("W3COS_DEVICE_NAME").unwrap_or_else(|_| "这台设备".to_string()),
        }
    }

    /// Loopback only. Tests use this so they never dial the public lists.
    pub fn local_test() -> Self {
        Self {
            bootstrap: Vec::new(),
            relays: Vec::new(),
            stun: Vec::new(),
            nostr: Vec::new(),
            enable_mdns: false,
            enable_dht: true,
            enable_nostr: false,
            enable_relay_server: true,
            enable_dcutr: true,
            dial_public: false,
            listen: vec!["/ip4/127.0.0.1/udp/0/quic-v1".to_string()],
            name: "test".to_string(),
        }
    }

    pub fn from_env() -> Self {
        let disable = std::env::var("W3COS_P2P_DISABLE_DEFAULTS").ok().as_deref() == Some("1");
        let file = std::env::var("W3COS_P2P_CONFIG").ok();
        let file_text = file
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        Self::with_overrides(
            disable,
            file_text.as_deref(),
            &[
                ("bootstrap", std::env::var("W3COS_P2P_BOOTSTRAP").ok()),
                ("relays", std::env::var("W3COS_P2P_RELAYS").ok()),
                ("stun", std::env::var("W3COS_P2P_STUN").ok()),
                ("nostr", std::env::var("W3COS_P2P_NOSTR").ok()),
            ],
        )
    }

    pub fn with_overrides(
        disable_defaults: bool,
        file_json: Option<&str>,
        csv: &[(&str, Option<String>)],
    ) -> Self {
        let mut config = if disable_defaults {
            Self {
                bootstrap: Vec::new(),
                relays: Vec::new(),
                stun: Vec::new(),
                nostr: Vec::new(),
                ..Self::production()
            }
        } else {
            Self::production()
        };
        config.dial_public = !config.bootstrap.is_empty() || !config.relays.is_empty();
        if let Some(text) = file_json
            && let Ok(extra) = serde_json::from_str::<ExtraNodes>(text)
        {
            config.append(extra);
        }
        for (kind, raw) in csv {
            let Some(raw) = raw else {
                continue;
            };
            let items = split_csv(raw);
            match *kind {
                "bootstrap" => config.bootstrap.extend(items),
                "relays" => config.relays.extend(items),
                "stun" => config.stun.extend(items),
                "nostr" => config.nostr.extend(items),
                _ => {}
            }
        }
        config.bootstrap = dedup(config.bootstrap);
        config.relays = dedup(config.relays);
        config.stun = dedup(config.stun);
        config.nostr = dedup(config.nostr);
        config.dial_public = !config.bootstrap.is_empty() || !config.relays.is_empty();
        config
    }

    pub fn append(&mut self, extra: ExtraNodes) {
        self.bootstrap.extend(extra.bootstrap);
        self.relays.extend(extra.relays);
        self.stun.extend(extra.stun);
        self.nostr.extend(extra.nostr);
    }
}

pub fn default_bootstrap() -> Vec<String> {
    vec![
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".into(),
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa".into(),
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb".into(),
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt".into(),
        "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ".into(),
        "/ip4/104.131.131.82/udp/4001/quic-v1/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ"
            .into(),
    ]
}

/// Historical public Circuit Relay candidates (distinct addresses). Any peer
/// that advertises the hop protocol, including this user's other devices, is
/// added at runtime. This is not a TURN provider.
pub fn default_relays() -> Vec<String> {
    vec![
        "/ip4/147.75.80.110/tcp/4001/p2p/QmbFgm5zan8P6eWWmeyfncR5feYEMPbht5b1FW1C37aQ7y".into(),
        "/ip4/147.75.195.153/tcp/4001/p2p/QmW9m57aiBDHAkKj9nmFSEn7ZqrcF1fZS4bipsTCHburei".into(),
        "/ip4/147.75.70.221/tcp/4001/p2p/Qme8g49gm3q4Acp7xWBKg3nAa9fxZ1YmyDJdyGgoG6LsXh".into(),
    ]
}

pub fn default_stun() -> Vec<String> {
    vec![
        "stun.cloudflare.com:3478".into(),
        "stun.l.google.com:19302".into(),
        "stun.services.mozilla.com:3478".into(),
    ]
}

pub fn default_nostr() -> Vec<String> {
    vec![
        "wss://relay.damus.io".into(),
        "wss://nos.lol".into(),
        "wss://relay.nostr.band".into(),
        "wss://nostr.wine".into(),
    ]
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn dedup(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        if !out.iter().any(|seen: &String| seen == &item) {
            out.push(item);
        }
    }
    out
}

fn peer_ids(addrs: &[String]) -> Vec<String> {
    addrs
        .iter()
        .filter_map(|addr| addr.rsplit("/p2p/").next())
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_lists_and_added_nodes_are_kept() {
        let config = NetConfig::production();
        assert!(config.bootstrap.len() >= 4, "{:?}", config.bootstrap);
        assert!(config.relays.len() >= 3, "{:?}", config.relays);
        assert!(config.stun.len() >= 3);
        assert!(config.nostr.len() >= 2);
        let boot_ids = peer_ids(&config.bootstrap);
        assert!(boot_ids.len() >= 4);
        assert!(
            boot_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                >= 4,
            "bootstrap peer ids must not collapse to one provider"
        );
        assert_ne!(config.relays[0], config.relays[1]);
        assert!(config.stun.iter().any(|item| item.contains("cloudflare")));
        assert!(config.stun.iter().any(|item| item.contains("google")));
        assert!(config.nostr.iter().any(|item| item.contains("damus")));
        assert!(config.nostr.iter().any(|item| item.contains("nos.lol")));

        let merged = NetConfig::with_overrides(
            false,
            Some(
                r#"{"bootstrap":["/ip4/10.1.2.3/tcp/4001/p2p/QmSelfHostedPeer"],"relays":["/ip4/10.9.9.9/tcp/4001/p2p/QmCommunityRelay"],"stun":["stun.example.org:3478"],"nostr":["wss://relay.example.org"]}"#,
            ),
            &[
                (
                    "bootstrap",
                    Some("/ip4/192.0.2.8/tcp/4001/p2p/QmExtra".into()),
                ),
                ("relays", None),
                ("stun", None),
                ("nostr", None),
            ],
        );
        assert!(merged.bootstrap.len() > config.bootstrap.len());
        assert!(
            merged
                .bootstrap
                .iter()
                .any(|item| item.contains("QmSelfHostedPeer"))
        );
        assert!(merged.bootstrap.iter().any(|item| item.contains("QmExtra")));
        assert!(
            merged
                .relays
                .iter()
                .any(|item| item.contains("QmCommunityRelay"))
        );
        assert!(
            merged
                .stun
                .iter()
                .any(|item| item == "stun.example.org:3478")
        );
        assert!(
            merged
                .nostr
                .iter()
                .any(|item| item == "wss://relay.example.org")
        );

        let only_user = NetConfig::with_overrides(
            true,
            None,
            &[
                (
                    "bootstrap",
                    Some("/ip4/192.0.2.9/tcp/4001/p2p/QmOnly".into()),
                ),
                ("relays", None),
                ("stun", None),
                ("nostr", None),
            ],
        );
        assert_eq!(only_user.bootstrap.len(), 1);
        assert!(only_user.relays.is_empty());
    }
}
