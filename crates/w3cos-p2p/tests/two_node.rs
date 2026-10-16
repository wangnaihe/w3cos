//! Two local nodes. Nothing here dials the public bootstrap or relay lists.

use std::time::{Duration, Instant};

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use w3cos_p2p::hpke;
use w3cos_p2p::{Endpoint, LinkMode, NetConfig};

fn seed(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn local(name: &str) -> NetConfig {
    let mut config = NetConfig::local_test();
    config.name = name.to_string();
    config.enable_dcutr = false;
    config.enable_nostr = false;
    config.dial_public = false;
    config
}

fn seal_between(sender: &Endpoint, recipient: &Endpoint, plain: &[u8]) -> Vec<u8> {
    hpke::seal(
        &recipient.x25519_public(),
        &sender.x25519_public(),
        plain,
        [9; 32],
    )
    .expect("seal")
}

#[test]
fn two_nodes_exchange_sealed_bytes_over_quic() {
    let a = Endpoint::spawn(local("书桌"), seed(1)).expect("a");
    let b = Endpoint::spawn(local("手机"), seed(2)).expect("b");
    let listen = a.wait_listen(Duration::from_secs(15)).expect("listen");
    b.dial(listen.with(Protocol::P2p(a.peer_id())));
    assert!(
        b.wait_link(a.peer_id(), LinkMode::Direct, Duration::from_secs(20)),
        "direct link did not come up"
    );
    let plain = b"draft-secret-not-on-the-wire";
    let sealed = seal_between(&b, &a, plain);
    assert!(
        !sealed.windows(plain.len()).any(|window| window == plain),
        "plaintext leaked into the datagram"
    );
    assert!(b.send(a.peer_id(), sealed.clone()));
    let got = a.recv_timeout(Duration::from_secs(15)).expect("inbound");
    assert_eq!(got, sealed);
    let opened = hpke::open(&seed(1), &b.x25519_public(), &got).expect("open");
    assert_eq!(opened, plain);
    assert_eq!(a.link(&b.peer_id()), LinkMode::Direct);
}

#[test]
fn a_circuit_relay_carries_ciphertext_and_stays_a_relay() {
    let relay = Endpoint::spawn(local("中继"), seed(9)).expect("relay");
    let listen = relay
        .wait_listen(Duration::from_secs(15))
        .expect("relay listen");
    let relay_addr = listen.with(Protocol::P2p(relay.peer_id())).to_string();

    let mut hidden = local("书桌");
    hidden.listen.clear();
    hidden.relays = vec![relay_addr.clone()];
    let mut dialer = local("手机");
    dialer.relays = vec![relay_addr];
    let a = Endpoint::spawn(hidden, seed(4)).expect("hidden");
    let b = Endpoint::spawn(dialer, seed(5)).expect("dialer");

    let circuit = a
        .wait_circuit(Duration::from_secs(25))
        .expect("reservation");
    let target = circuit_to(circuit, a.peer_id());
    b.dial(target);
    assert!(
        b.wait_link(a.peer_id(), LinkMode::Relay, Duration::from_secs(25)),
        "expected a relayed link, got {:?}",
        b.link(&a.peer_id())
    );
    assert!(
        a.wait_link(b.peer_id(), LinkMode::Relay, Duration::from_secs(10)),
        "hidden node did not see the relay, got {:?}",
        a.link(&b.peer_id())
    );

    let plain = b"still-sealed-across-the-relay";
    let sealed = seal_between(&b, &a, plain);
    assert!(!sealed.windows(plain.len()).any(|window| window == plain));
    assert!(b.send(a.peer_id(), sealed.clone()));
    let got = a
        .recv_timeout(Duration::from_secs(15))
        .expect("relayed inbound");
    assert_eq!(got, sealed);
    assert_eq!(
        hpke::open(&seed(4), &b.x25519_public(), &got).expect("open"),
        plain
    );
}

#[test]
fn a_moved_address_is_resolved_from_the_dht() {
    let a = Endpoint::spawn(local("书桌"), seed(6)).expect("a");
    let b = Endpoint::spawn(local("手机"), seed(7)).expect("b");
    let listen = a.wait_listen(Duration::from_secs(15)).expect("listen");
    b.dial(listen.with(Protocol::P2p(a.peer_id())));
    assert!(b.wait_link(a.peer_id(), LinkMode::Direct, Duration::from_secs(20)));

    let moved = format!("/ip4/203.0.113.50/udp/4001/quic-v1/p2p/{}", a.peer_id());
    a.set_extra_addrs(vec![moved.clone()]);
    a.publish();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut found = None;
    while Instant::now() < deadline {
        if let Some(card) = b.lookup(&a.fingerprint(), a.ed25519_public(), Duration::from_secs(4)) {
            found = Some(card);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let card = found.expect("dht presence");
    assert!(
        card.presence.addrs.iter().any(|addr| addr == &moved),
        "published address missing: {:?}",
        card.presence.addrs
    );
    assert!(
        !card
            .body
            .windows(b"draft".len())
            .any(|window| window == b"draft")
    );
}

fn circuit_to(circuit: Multiaddr, peer: libp2p::PeerId) -> Multiaddr {
    let text = circuit.to_string();
    if text.contains(&peer.to_string()) {
        circuit
    } else {
        circuit.with(Protocol::P2p(peer))
    }
}
