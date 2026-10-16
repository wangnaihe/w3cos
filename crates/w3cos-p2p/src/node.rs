//! libp2p swarm: QUIC, mDNS, Kademlia, AutoNAT, DCUtR, and Circuit Relay v2.
//!
//! Handoff bytes are already sealed when they reach `send`. The relay forwards
//! that ciphertext and never sees the draft.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::core::ConnectedPoint;
use libp2p::kad::store::MemoryStore;
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport, cbor};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::{ConnectionId, NetworkBehaviour, SwarmEvent};
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder, autonat, dcutr, identify, kad, mdns,
    noise, ping, relay, yamux,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::NetConfig;
use crate::identity::DeviceIdentity;
use crate::presence::{self, Presence, SignedPresence};
use crate::reconnect::{self, ResolveStep};
use crate::{LinkMode, hex_encode};

const HANDOFF: &str = "/w3cos/handoff/1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct HandoffRequest {
    ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct HandoffResponse {
    ok: bool,
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    mdns: Toggle<mdns::tokio::Behaviour>,
    kad: kad::Behaviour<MemoryStore>,
    autonat: autonat::Behaviour,
    dcutr: Toggle<dcutr::Behaviour>,
    relay_server: relay::Behaviour,
    relay_client: relay::client::Behaviour,
    handoff: cbor::Behaviour<HandoffRequest, HandoffResponse>,
}

struct Snap {
    peer_id: PeerId,
    listen: Vec<Multiaddr>,
    circuit: Vec<Multiaddr>,
    links: HashMap<PeerId, LinkMode>,
    changes: VecDeque<(String, LinkMode)>,
    inbound: VecDeque<Vec<u8>>,
    acks: VecDeque<String>,
    failures: VecDeque<String>,
    error: Option<String>,
    observed: Vec<String>,
}

struct Tracked {
    fingerprint: String,
    ed25519: [u8; 32],
    remembered: Vec<String>,
    local_changed: bool,
    remote_failed: bool,
    plan_at: usize,
}

enum Cmd {
    Send {
        peer: PeerId,
        bytes: Vec<u8>,
    },
    Dial(Multiaddr),
    Track {
        peer: PeerId,
        fingerprint: String,
        ed25519: [u8; 32],
    },
    Lookup {
        fingerprint: String,
        ed25519: [u8; 32],
        reply: tokio::sync::oneshot::Sender<Option<Vec<u8>>>,
    },
    Publish,
    SetExtra(Vec<String>),
    NoteLocalChanged,
    Resolve {
        peer: PeerId,
        fingerprint: String,
        ed25519: [u8; 32],
    },
}

pub struct Endpoint {
    peer_id: PeerId,
    x25519_public: [u8; 32],
    ed25519_public: [u8; 32],
    fingerprint: String,
    seed: [u8; 32],
    cmd: mpsc::Sender<Cmd>,
    snap: Arc<Mutex<Snap>>,
    cv: Arc<Condvar>,
}

impl Endpoint {
    pub fn spawn(config: NetConfig, seed: [u8; 32]) -> Result<Self, String> {
        let identity = DeviceIdentity::from_seed(seed);
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let snap = Arc::new(Mutex::new(Snap {
            peer_id: identity.peer_id,
            listen: Vec::new(),
            circuit: Vec::new(),
            links: HashMap::new(),
            changes: VecDeque::new(),
            inbound: VecDeque::new(),
            acks: VecDeque::new(),
            failures: VecDeque::new(),
            error: None,
            observed: Vec::new(),
        }));
        let cv = Arc::new(Condvar::new());
        let endpoint = Self {
            peer_id: identity.peer_id,
            x25519_public: identity.x25519_public,
            ed25519_public: identity.ed25519_public,
            fingerprint: identity.fingerprint(),
            seed,
            cmd: cmd_tx,
            snap: Arc::clone(&snap),
            cv: Arc::clone(&cv),
        };
        std::thread::Builder::new()
            .name("w3cos-p2p".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        snap.lock().expect("snap").error = Some(err.to_string());
                        cv.notify_all();
                        return;
                    }
                };
                runtime.block_on(run(identity, config, cmd_rx, snap, cv));
            })
            .map_err(|err| err.to_string())?;
        Ok(endpoint)
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn x25519_public(&self) -> [u8; 32] {
        self.x25519_public
    }

    pub fn ed25519_public(&self) -> [u8; 32] {
        self.ed25519_public
    }

    pub fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    pub fn dial(&self, addr: Multiaddr) {
        let _ = self.cmd.blocking_send(Cmd::Dial(addr));
    }

    pub fn track(&self, peer: PeerId, fingerprint: &str, ed25519: [u8; 32]) {
        let _ = self.cmd.blocking_send(Cmd::Track {
            peer,
            fingerprint: fingerprint.to_string(),
            ed25519,
        });
    }

    pub fn send(&self, peer: PeerId, bytes: Vec<u8>) -> bool {
        self.cmd.blocking_send(Cmd::Send { peer, bytes }).is_ok()
    }

    pub fn publish(&self) {
        let _ = self.cmd.blocking_send(Cmd::Publish);
    }

    pub fn set_extra_addrs(&self, addrs: Vec<String>) {
        let _ = self.cmd.blocking_send(Cmd::SetExtra(addrs));
    }

    pub fn note_local_changed(&self) {
        let _ = self.cmd.blocking_send(Cmd::NoteLocalChanged);
    }

    /// Look the paired device up on the DHT, then Nostr, and dial what verifies.
    pub fn resolve(&self, peer: PeerId, fingerprint: &str, ed25519: [u8; 32]) {
        let _ = self.cmd.blocking_send(Cmd::Resolve {
            peer,
            fingerprint: fingerprint.to_string(),
            ed25519,
        });
    }

    pub fn lookup(
        &self,
        fingerprint: &str,
        ed25519: [u8; 32],
        timeout: Duration,
    ) -> Option<SignedPresence> {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.cmd
            .blocking_send(Cmd::Lookup {
                fingerprint: fingerprint.to_string(),
                ed25519,
                reply: tx,
            })
            .ok()?;
        let deadline = Instant::now() + timeout;
        loop {
            match rx.try_recv() {
                Ok(bytes) => {
                    let bytes = bytes?;
                    let signed = presence::decode(&bytes)?;
                    return presence::verify(&ed25519, &signed).then_some(signed);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => return None,
            }
        }
    }

    pub fn wait_listen(&self, timeout: Duration) -> Option<Multiaddr> {
        self.wait(timeout, |snap| {
            snap.listen
                .iter()
                .find(|addr| {
                    let text = addr.to_string();
                    text.contains("quic-v1") && !text.contains("p2p-circuit")
                })
                .cloned()
        })
    }

    pub fn wait_circuit(&self, timeout: Duration) -> Option<Multiaddr> {
        self.wait(timeout, |snap| snap.circuit.first().cloned())
    }

    pub fn wait_link(&self, peer: PeerId, mode: LinkMode, timeout: Duration) -> bool {
        self.wait(timeout, |snap| {
            snap.links
                .get(&peer)
                .filter(|current| **current == mode)
                .copied()
        })
        .is_some()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.snap.lock().expect("snap");
        loop {
            if let Some(bytes) = guard.inbound.pop_front() {
                return Some(bytes);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let (next, _) = self
                .cv
                .wait_timeout(guard, deadline.saturating_duration_since(Instant::now()))
                .expect("snap");
            guard = next;
        }
    }

    pub fn drain(&self) -> Drain {
        let mut guard = self.snap.lock().expect("snap");
        Drain {
            inbound: guard.inbound.drain(..).collect(),
            changes: guard.changes.drain(..).collect(),
            acks: guard.acks.drain(..).collect(),
            failures: guard.failures.drain(..).collect(),
        }
    }

    pub fn link(&self, peer: &PeerId) -> LinkMode {
        self.snap
            .lock()
            .expect("snap")
            .links
            .get(peer)
            .copied()
            .unwrap_or(LinkMode::Offline)
    }

    fn wait<T>(&self, timeout: Duration, pick: impl Fn(&Snap) -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.snap.lock().expect("snap");
        loop {
            if let Some(error) = &guard.error {
                let _ = error;
            }
            if let Some(found) = pick(&guard) {
                return Some(found);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let (next, _) = self
                .cv
                .wait_timeout(guard, deadline.saturating_duration_since(Instant::now()))
                .expect("snap");
            guard = next;
        }
    }
}

pub struct Drain {
    pub inbound: Vec<Vec<u8>>,
    pub changes: Vec<(String, LinkMode)>,
    pub acks: Vec<String>,
    pub failures: Vec<String>,
}

async fn run(
    identity: DeviceIdentity,
    config: NetConfig,
    mut cmds: mpsc::Receiver<Cmd>,
    snap: Arc<Mutex<Snap>>,
    cv: Arc<Condvar>,
) {
    let swarm = match build_swarm(&identity, &config) {
        Ok(swarm) => swarm,
        Err(err) => {
            snap.lock().expect("snap").error = Some(err);
            cv.notify_all();
            return;
        }
    };
    let (note_tx, mut notes) = mpsc::unbounded_channel();
    let mut drive = Drive {
        swarm,
        config,
        snap,
        cv,
        tracked: HashMap::new(),
        lookups: HashMap::new(),
        conns: HashMap::new(),
        note_tx,
        extra_addrs: Vec::new(),
        seed: identity_seed(&identity),
        fingerprint: identity.fingerprint(),
        x25519_hex: hex_encode(&identity.x25519_public),
        ed25519: identity.ed25519_public,
        name_peer: identity.peer_id,
        local_changed: false,
    };
    let _ = identity;
    drive.listen();
    drive.dial_configured();
    let mut publish = tokio::time::interval(Duration::from_secs(45));
    publish.tick().await;
    loop {
        tokio::select! {
            event = drive.swarm.select_next_some() => drive.on_swarm(event),
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                drive.on_cmd(cmd).await;
            }
            note = notes.recv() => {
                let Some(note) = note else { break };
                drive.on_note(note);
            }
            _ = publish.tick() => drive.publish_presence(),
        }
    }
}

struct Pending {
    ed25519: [u8; 32],
    fingerprint: String,
    reply: Option<tokio::sync::oneshot::Sender<Option<Vec<u8>>>>,
    dial: Option<PeerId>,
}

enum Note {
    Dial { peer: PeerId, addrs: Vec<String> },
}

fn identity_seed(identity: &DeviceIdentity) -> [u8; 32] {
    identity.x25519_secret
}

struct Drive {
    swarm: Swarm<Behaviour>,
    config: NetConfig,
    snap: Arc<Mutex<Snap>>,
    cv: Arc<Condvar>,
    tracked: HashMap<PeerId, Tracked>,
    lookups: HashMap<kad::QueryId, Pending>,
    conns: HashMap<PeerId, HashMap<ConnectionId, LinkMode>>,
    note_tx: mpsc::UnboundedSender<Note>,
    extra_addrs: Vec<String>,
    seed: [u8; 32],
    fingerprint: String,
    x25519_hex: String,
    ed25519: [u8; 32],
    name_peer: PeerId,
    local_changed: bool,
}

impl Drive {
    fn listen(&mut self) {
        for addr in &self.config.listen.clone() {
            let Ok(multi) = addr.parse::<Multiaddr>() else {
                continue;
            };
            if let Err(err) = self.swarm.listen_on(multi) {
                let mut snap = self.snap.lock().expect("snap");
                if snap.listen.is_empty() {
                    snap.error = Some(err.to_string());
                }
            }
        }
    }

    fn dial_configured(&mut self) {
        if !self.config.dial_public
            && self.config.relays.is_empty()
            && self.config.bootstrap.is_empty()
        {
            return;
        }
        let relays = self.config.relays.clone();
        let bootstrap = if self.config.dial_public {
            self.config.bootstrap.clone()
        } else {
            Vec::new()
        };
        for addr in bootstrap.iter().chain(relays.iter()) {
            self.dial_text(addr);
        }
        for addr in &relays {
            self.reserve_text(addr);
        }
    }

    fn dial_text(&mut self, addr: &str) {
        let Ok(multi) = addr.parse::<Multiaddr>() else {
            return;
        };
        if self.config.enable_dht
            && let Some(peer) = peer_of(&multi)
        {
            self.swarm
                .behaviour_mut()
                .kad
                .add_address(&peer, multi.clone());
        }
        let _ = self.swarm.dial(multi);
    }

    fn reserve_text(&mut self, addr: &str) {
        let Ok(multi) = addr.parse::<Multiaddr>() else {
            return;
        };
        let circuit = multi.with(Protocol::P2pCircuit);
        let _ = self.swarm.listen_on(circuit);
    }

    fn on_cmd(
        &mut self,
        cmd: Cmd,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(self.on_cmd_inner(cmd))
    }

    async fn on_cmd_inner(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Send { peer, bytes } => {
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .handoff
                    .send_request(&peer, HandoffRequest { ciphertext: bytes });
            }
            Cmd::Dial(addr) => {
                let _ = self.swarm.dial(addr);
            }
            Cmd::Track {
                peer,
                fingerprint,
                ed25519,
            } => {
                self.tracked.insert(
                    peer,
                    Tracked {
                        fingerprint,
                        ed25519,
                        remembered: Vec::new(),
                        local_changed: self.local_changed,
                        remote_failed: false,
                        plan_at: 0,
                    },
                );
                let _ = self.swarm.dial(peer);
            }
            Cmd::Lookup {
                fingerprint,
                ed25519,
                reply,
            } => self.start_lookup(fingerprint, ed25519, reply).await,
            Cmd::Publish => self.publish_presence(),
            Cmd::SetExtra(addrs) => {
                self.extra_addrs = addrs;
                self.publish_presence();
            }
            Cmd::NoteLocalChanged => {
                self.local_changed = true;
                for tracked in self.tracked.values_mut() {
                    tracked.local_changed = true;
                    tracked.plan_at = 0;
                }
            }
            Cmd::Resolve {
                peer,
                fingerprint,
                ed25519,
            } => {
                let local_changed = self.local_changed;
                {
                    let tracked = self.tracked.entry(peer).or_insert_with(|| Tracked {
                        fingerprint: fingerprint.clone(),
                        ed25519,
                        remembered: Vec::new(),
                        local_changed,
                        remote_failed: true,
                        plan_at: 0,
                    });
                    tracked.fingerprint = fingerprint;
                    tracked.ed25519 = ed25519;
                    tracked.remote_failed = true;
                }
                self.apply_step(peer, Some(ResolveStep::Kad));
            }
        }
    }

    async fn start_lookup(
        &mut self,
        fingerprint: String,
        ed25519: [u8; 32],
        reply: tokio::sync::oneshot::Sender<Option<Vec<u8>>>,
    ) {
        if !self.config.enable_dht {
            let found = self.nostr_fetch(&fingerprint).await;
            let _ = reply.send(found.filter(|bytes| card_ok(&ed25519, bytes)));
            return;
        }
        let key = kad::RecordKey::new(&presence::record_key(&fingerprint));
        let query = self.swarm.behaviour_mut().kad.get_record(key);
        self.lookups.insert(
            query,
            Pending {
                ed25519,
                fingerprint,
                reply: Some(reply),
                dial: None,
            },
        );
    }

    async fn nostr_fetch(&self, fingerprint: &str) -> Option<Vec<u8>> {
        if !self.config.enable_nostr || self.config.nostr.is_empty() {
            return None;
        }
        let relays = self.config.nostr.clone();
        let fingerprint = fingerprint.to_string();
        tokio::task::spawn_blocking(move || {
            crate::nostr::fetch_latest(&fingerprint, &relays, Duration::from_secs(4))
                .map(|signed| presence::encode(&signed))
        })
        .await
        .ok()
        .flatten()
    }

    fn publish_presence(&mut self) {
        let addrs = self.shareable_addrs();
        if addrs.is_empty() {
            return;
        }
        let presence = Presence {
            v: 1,
            name: self.config.name.clone(),
            fp: self.fingerprint.clone(),
            x25519: self.x25519_hex.clone(),
            ed25519: hex_encode(&self.ed25519),
            peer: self.name_peer.to_string(),
            addrs,
            ts: unix_now(),
        };
        let identity = DeviceIdentity::from_seed(self.seed);
        let Some(signed) = presence::sign(&identity.keypair, &presence) else {
            return;
        };
        let bytes = presence::encode(&signed);
        if self.config.enable_dht {
            let record = kad::Record {
                key: kad::RecordKey::new(&presence::record_key(&self.fingerprint)),
                value: bytes.clone(),
                publisher: Some(self.name_peer),
                expires: None,
            };
            let _ = self
                .swarm
                .behaviour_mut()
                .kad
                .put_record(record, kad::Quorum::One);
        }
        if self.config.enable_nostr {
            let content = String::from_utf8_lossy(&bytes).to_string();
            let seed = self.seed;
            let fingerprint = self.fingerprint.clone();
            let relays = self.config.nostr.clone();
            tokio::spawn(async move {
                let _ = tokio::task::spawn_blocking(move || {
                    crate::nostr::publish_presence(
                        &seed,
                        &fingerprint,
                        &content,
                        &relays,
                        Duration::from_secs(4),
                    )
                })
                .await;
            });
        }
    }

    fn shareable_addrs(&self) -> Vec<String> {
        let snap = self.snap.lock().expect("snap");
        let mut addrs = Vec::new();
        for addr in snap.listen.iter().chain(snap.circuit.iter()) {
            let text = addr.to_string();
            if text.contains("/ip4/0.0.0.0/") || text.contains("/ip6/::/") {
                continue;
            }
            addrs.push(annotate(addr, self.name_peer));
        }
        addrs.extend(snap.observed.clone());
        drop(snap);
        addrs.extend(self.extra_addrs.clone());
        let mut unique = Vec::new();
        for addr in addrs {
            if !unique.contains(&addr) {
                unique.push(addr);
            }
        }
        unique
    }

    fn on_swarm(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                let circuit = address
                    .iter()
                    .any(|item| matches!(item, Protocol::P2pCircuit));
                let mut snap = self.snap.lock().expect("snap");
                if circuit {
                    if !snap.circuit.contains(&address) {
                        snap.circuit.push(address);
                    }
                } else if !snap.listen.contains(&address) {
                    snap.listen.push(address);
                }
                snap.error = None;
                drop(snap);
                self.cv.notify_all();
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                let addr = endpoint_addr(&endpoint);
                let mode = if connection_is_relayed(&endpoint) {
                    LinkMode::Relay
                } else {
                    LinkMode::Direct
                };
                if self.config.enable_dht {
                    self.swarm
                        .behaviour_mut()
                        .kad
                        .add_address(&peer_id, addr.clone());
                }
                if let Some(tracked) = self.tracked.get_mut(&peer_id) {
                    let text = annotate(addr, peer_id);
                    if !tracked.remembered.contains(&text) {
                        tracked.remembered.push(text);
                    }
                    tracked.remote_failed = false;
                }
                self.set_link(peer_id, connection_id, Some(mode));
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                ..
            } => {
                self.set_link(peer_id, connection_id, None);
                if self.link_mode(peer_id) == LinkMode::Offline {
                    self.begin_reconnect(peer_id);
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, .. } => {
                if let Some(peer_id) = peer_id
                    && let Some(tracked) = self.tracked.get_mut(&peer_id)
                {
                    tracked.remote_failed = true;
                    self.advance_reconnect(peer_id);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Handoff(event)) => self.on_handoff(event),
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                let mut snap = self.snap.lock().expect("snap");
                let observed = info.observed_addr.to_string();
                if !snap.observed.contains(&observed) {
                    snap.observed.push(observed);
                }
                drop(snap);
                self.swarm.add_external_address(info.observed_addr.clone());
                let hop = info
                    .protocols
                    .iter()
                    .any(|protocol| protocol == &relay::HOP_PROTOCOL_NAME);
                let wanted = self
                    .config
                    .relays
                    .iter()
                    .any(|addr| peer_of_str(addr) == Some(peer_id))
                    || self.tracked.contains_key(&peer_id);
                if hop && wanted {
                    for addr in info.listen_addrs {
                        let with_peer = if peer_of(&addr).is_some() {
                            addr
                        } else {
                            addr.with(Protocol::P2p(peer_id))
                        };
                        let _ = self.swarm.listen_on(with_peer.with(Protocol::P2pCircuit));
                    }
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                for (peer, addr) in peers {
                    let _ = self.swarm.dial(addr.with(Protocol::P2p(peer)));
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id,
                result,
                ..
            })) => self.on_kad(id, result),
            SwarmEvent::NewExternalAddrCandidate { address } => {
                let text = address.to_string();
                let mut snap = self.snap.lock().expect("snap");
                if !snap.observed.contains(&text) {
                    snap.observed.push(text);
                }
            }
            _ => {}
        }
    }

    fn on_handoff(&mut self, event: request_response::Event<HandoffRequest, HandoffResponse>) {
        match event {
            request_response::Event::Message { peer, message, .. } => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    let mut snap = self.snap.lock().expect("snap");
                    snap.inbound.push_back(request.ciphertext);
                    drop(snap);
                    self.cv.notify_all();
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .handoff
                        .send_response(channel, HandoffResponse { ok: true });
                }
                request_response::Message::Response { response, .. } => {
                    if response.ok {
                        let mut snap = self.snap.lock().expect("snap");
                        snap.acks.push_back(peer.to_string());
                        drop(snap);
                        self.cv.notify_all();
                    }
                }
            },
            request_response::Event::OutboundFailure { peer, .. } => {
                let mut snap = self.snap.lock().expect("snap");
                snap.failures.push_back(peer.to_string());
                drop(snap);
                self.cv.notify_all();
            }
            _ => {}
        }
    }

    fn on_kad(&mut self, id: kad::QueryId, result: kad::QueryResult) {
        let kad::QueryResult::GetRecord(found) = result else {
            return;
        };
        if !self.lookups.contains_key(&id) {
            return;
        }
        match found {
            Ok(kad::GetRecordOk::FoundRecord(record)) => {
                let Some(pending) = self.lookups.remove(&id) else {
                    return;
                };
                let bytes = record.record.value;
                if card_ok(&pending.ed25519, &bytes) {
                    self.finish_lookup(pending, Some(bytes));
                } else {
                    self.miss_lookup(pending);
                }
            }
            Ok(_) | Err(_) => {
                let Some(pending) = self.lookups.remove(&id) else {
                    return;
                };
                self.miss_lookup(pending);
            }
        }
    }

    fn finish_lookup(&mut self, pending: Pending, bytes: Option<Vec<u8>>) {
        if let (Some(peer), Some(bytes)) = (pending.dial, bytes.as_ref())
            && let Some(signed) = presence::decode(bytes)
        {
            for addr in reconnect::prefer_paths(&signed.presence.addrs) {
                self.dial_text(&addr);
            }
            let _ = peer;
        }
        if let Some(reply) = pending.reply {
            let _ = reply.send(bytes);
        }
    }

    fn miss_lookup(&mut self, pending: Pending) {
        if !self.config.enable_nostr || self.config.nostr.is_empty() {
            if let Some(reply) = pending.reply {
                let _ = reply.send(None);
            }
            return;
        }
        let relays = self.config.nostr.clone();
        let fingerprint = pending.fingerprint.clone();
        let ed25519 = pending.ed25519;
        let note_tx = self.note_tx.clone();
        let dial = pending.dial;
        let reply = pending.reply;
        tokio::spawn(async move {
            let found = tokio::task::spawn_blocking(move || {
                crate::nostr::fetch_latest(&fingerprint, &relays, Duration::from_secs(4))
            })
            .await
            .ok()
            .flatten();
            let bytes = found
                .filter(|signed| presence::verify(&ed25519, signed))
                .map(|signed| presence::encode(&signed));
            if let (Some(peer), Some(raw)) = (dial, bytes.as_deref())
                && let Some(signed) = presence::decode(raw)
            {
                let _ = note_tx.send(Note::Dial {
                    peer,
                    addrs: reconnect::prefer_paths(&signed.presence.addrs),
                });
            }
            if let Some(reply) = reply {
                let _ = reply.send(bytes);
            }
        });
    }

    fn on_note(&mut self, note: Note) {
        match note {
            Note::Dial { addrs, .. } => {
                for addr in addrs {
                    self.dial_text(&addr);
                }
            }
        }
    }

    fn set_link(&mut self, peer: PeerId, connection: ConnectionId, mode: Option<LinkMode>) {
        let slots = self.conns.entry(peer).or_default();
        match mode {
            Some(mode) => {
                slots.insert(connection, mode);
            }
            None => {
                slots.remove(&connection);
            }
        }
        let next = fold_links(slots);
        if slots.is_empty() {
            self.conns.remove(&peer);
        }
        let mut snap = self.snap.lock().expect("snap");
        let previous = snap.links.get(&peer).copied();
        if next == LinkMode::Offline {
            snap.links.remove(&peer);
        } else {
            snap.links.insert(peer, next);
        }
        if previous != Some(next) && !(previous.is_none() && next == LinkMode::Offline) {
            snap.changes.push_back((peer.to_string(), next));
        }
        drop(snap);
        self.cv.notify_all();
    }

    fn link_mode(&self, peer: PeerId) -> LinkMode {
        self.snap
            .lock()
            .expect("snap")
            .links
            .get(&peer)
            .copied()
            .unwrap_or(LinkMode::Offline)
    }

    fn begin_reconnect(&mut self, peer: PeerId) {
        let plan = {
            let Some(tracked) = self.tracked.get_mut(&peer) else {
                return;
            };
            tracked.plan_at = 0;
            reconnect::plan_reconnect(tracked.local_changed, tracked.remote_failed)
        };
        self.apply_step(peer, plan.first().cloned());
    }

    fn advance_reconnect(&mut self, peer: PeerId) {
        let step = {
            let Some(tracked) = self.tracked.get_mut(&peer) else {
                return;
            };
            tracked.plan_at = tracked.plan_at.saturating_add(1);
            let plan = reconnect::plan_reconnect(tracked.local_changed, tracked.remote_failed);
            plan.get(tracked.plan_at).cloned()
        };
        self.apply_step(peer, step);
    }

    fn apply_step(&mut self, peer: PeerId, step: Option<ResolveStep>) {
        let Some(step) = step else {
            return;
        };
        match step {
            ResolveStep::Remembered => {
                let addrs = self
                    .tracked
                    .get(&peer)
                    .map(|tracked| reconnect::prefer_paths(&tracked.remembered))
                    .unwrap_or_default();
                for addr in addrs {
                    self.dial_text(&addr);
                }
            }
            ResolveStep::Kad => {
                let (fingerprint, ed25519) = {
                    let Some(tracked) = self.tracked.get(&peer) else {
                        return;
                    };
                    (tracked.fingerprint.clone(), tracked.ed25519)
                };
                let pending = Pending {
                    ed25519,
                    fingerprint: fingerprint.clone(),
                    reply: None,
                    dial: Some(peer),
                };
                if !self.config.enable_dht {
                    self.miss_lookup(pending);
                    return;
                }
                let key = kad::RecordKey::new(&presence::record_key(&fingerprint));
                let query = self.swarm.behaviour_mut().kad.get_record(key);
                self.lookups.insert(query, pending);
            }
            ResolveStep::Nostr => {
                let (fingerprint, ed25519) = {
                    let Some(tracked) = self.tracked.get(&peer) else {
                        return;
                    };
                    (tracked.fingerprint.clone(), tracked.ed25519)
                };
                self.miss_lookup(Pending {
                    ed25519,
                    fingerprint,
                    reply: None,
                    dial: Some(peer),
                });
            }
        }
    }
}

fn fold_links(slots: &HashMap<ConnectionId, LinkMode>) -> LinkMode {
    if slots.values().any(|mode| *mode == LinkMode::Direct) {
        LinkMode::Direct
    } else if slots.values().any(|mode| *mode == LinkMode::Relay) {
        LinkMode::Relay
    } else {
        LinkMode::Offline
    }
}

fn card_ok(ed25519: &[u8; 32], bytes: &[u8]) -> bool {
    presence::decode(bytes).is_some_and(|signed| presence::verify(ed25519, &signed))
}

fn annotate(addr: &Multiaddr, peer: PeerId) -> String {
    if peer_of(addr).is_some() {
        addr.to_string()
    } else {
        addr.clone().with(Protocol::P2p(peer)).to_string()
    }
}

fn endpoint_addr(endpoint: &ConnectedPoint) -> &Multiaddr {
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr,
    }
}

fn connection_is_relayed(endpoint: &ConnectedPoint) -> bool {
    let has_circuit =
        |addr: &Multiaddr| addr.iter().any(|item| matches!(item, Protocol::P2pCircuit));
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => has_circuit(address),
        ConnectedPoint::Listener {
            local_addr,
            send_back_addr,
        } => has_circuit(local_addr) || has_circuit(send_back_addr),
    }
}

fn peer_of(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|protocol| match protocol {
        Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

fn peer_of_str(addr: &str) -> Option<PeerId> {
    let multi: Multiaddr = addr.parse().ok()?;
    peer_of(&multi)
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn build_swarm(identity: &DeviceIdentity, config: &NetConfig) -> Result<Swarm<Behaviour>, String> {
    let peer_id = identity.peer_id;
    let mut kad_config = kad::Config::new(StreamProtocol::new("/ipfs/kad/1.0.0"));
    kad_config.set_query_timeout(Duration::from_secs(8));
    let store = MemoryStore::new(peer_id);
    let mut kad = kad::Behaviour::with_config(peer_id, store, kad_config);
    if config.enable_dht {
        kad.set_mode(Some(kad::Mode::Server));
    }
    let mdns = if config.enable_mdns {
        Toggle::from(mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id).ok())
    } else {
        Toggle::from(None)
    };
    let dcutr = if config.enable_dcutr {
        Toggle::from(Some(dcutr::Behaviour::new(peer_id)))
    } else {
        Toggle::from(None)
    };
    let relay_server = if config.enable_relay_server {
        relay::Behaviour::new(peer_id, relay::Config::default())
    } else {
        relay::Behaviour::new(
            peer_id,
            relay::Config {
                max_reservations: 0,
                max_circuits: 0,
                ..relay::Config::default()
            },
        )
    };
    let key = identity.keypair.clone();
    let swarm = SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_quic()
        .with_dns()
        .map_err(|err| err.to_string())?
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(|err| err.to_string())?
        .with_behaviour(|keypair, relay_client| Behaviour {
            ping: ping::Behaviour::new(ping::Config::new()),
            identify: identify::Behaviour::new(identify::Config::new(
                "/w3cos/1.0.0".to_string(),
                keypair.public(),
            )),
            mdns,
            kad,
            autonat: autonat::Behaviour::new(peer_id, autonat::Config::default()),
            dcutr,
            relay_server,
            relay_client,
            handoff: cbor::Behaviour::new(
                [(StreamProtocol::new(HANDOFF), ProtocolSupport::Full)],
                request_response::Config::default(),
            ),
        })
        .map_err(|err| err.to_string())?
        .build();
    Ok(swarm)
}
