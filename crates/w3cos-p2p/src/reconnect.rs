//! When a path dies, try what we already know, then the DHT, then Nostr.
//! If both the local and the remote address changed, the remembered path is
//! skipped: neither side can migrate a QUIC connection it can no longer address.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveStep {
    Remembered,
    Kad,
    Nostr,
}

/// `local_changed` is our observed address. `remote_changed` means the last
/// dial to the remembered address failed, or the peer told us it moved.
pub fn plan_reconnect(local_changed: bool, remote_changed: bool) -> Vec<ResolveStep> {
    let mut steps = Vec::new();
    if !(local_changed && remote_changed) {
        steps.push(ResolveStep::Remembered);
    }
    steps.push(ResolveStep::Kad);
    steps.push(ResolveStep::Nostr);
    steps
}

/// Direct QUIC addresses first. Circuit addresses stay as the fallback.
pub fn prefer_paths(addrs: &[String]) -> Vec<String> {
    let mut direct = Vec::new();
    let mut relayed = Vec::new();
    for addr in addrs {
        if addr.contains("/p2p-circuit") {
            relayed.push(addr.clone());
        } else {
            direct.push(addr.clone());
        }
    }
    direct.extend(relayed);
    direct
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_moving_skips_the_stale_path() {
        assert_eq!(
            plan_reconnect(true, true),
            vec![ResolveStep::Kad, ResolveStep::Nostr]
        );
        assert_eq!(
            plan_reconnect(true, false),
            vec![
                ResolveStep::Remembered,
                ResolveStep::Kad,
                ResolveStep::Nostr
            ]
        );
        assert_eq!(
            plan_reconnect(false, true),
            vec![
                ResolveStep::Remembered,
                ResolveStep::Kad,
                ResolveStep::Nostr
            ]
        );
        let ordered = prefer_paths(&[
            "/ip4/198.51.100.2/tcp/4001/p2p/QmR/p2p-circuit/p2p/QmA".into(),
            "/ip4/203.0.113.9/udp/4001/quic-v1/p2p/QmA".into(),
        ]);
        assert!(ordered[0].contains("quic-v1"));
        assert!(ordered[1].contains("p2p-circuit"));
    }
}
