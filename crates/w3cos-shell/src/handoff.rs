//! Device pairing and handoff on the local network.
//!
//! Discovery announces only a name and a key fingerprint. Receipts and an
//! unexecuted draft are sealed to the peer's device key after a person
//! compares the short code. Window geometry is not part of the payload, and
//! a confirmation on one device does not run on the other.

use std::cell::RefCell;
use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

const CODE_TTL_SECS: u64 = 90;
const DISCOVERY_PORT: u16 = 47623;
const CLIP_HOLD_SECS: u64 = 600;
/// How long a send waits for the other device's ack before the no-answer copy.
const NO_ANSWER_AFTER: u64 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceKey {
    secret: [u8; 32],
    public: [u8; 32],
}

impl DeviceKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let secret = StaticSecret::from(seed);
        let public = PublicKey::from(&secret);
        Self {
            secret: seed,
            public: public.to_bytes(),
        }
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        self.public
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }
}

pub fn fingerprint(public: &[u8; 32]) -> String {
    let mut out = String::from("did:key:z");
    for byte in public {
        out.push(char::from(b'a' + (byte % 26)));
    }
    out
}

/// Six digits, grouped, plus a spaced form a screen reader can speak one by one.
pub fn short_code(local_fp: &str, peer_fp: &str, nonce: &str) -> (String, String) {
    let mut material = String::new();
    if local_fp <= peer_fp {
        material.push_str(local_fp);
        material.push('\n');
        material.push_str(peer_fp);
    } else {
        material.push_str(peer_fp);
        material.push('\n');
        material.push_str(local_fp);
    }
    material.push('\n');
    material.push_str(nonce);
    let digest = Sha256::digest(material.as_bytes());
    let mut digits = String::new();
    for byte in digest {
        digits.push(char::from(b'0' + (byte % 10)));
        if digits.len() == 6 {
            break;
        }
    }
    let grouped = format!("{} {}", &digits[..3], &digits[3..]);
    let spaced: String = digits.chars().flat_map(|ch| [ch, ' ']).collect();
    (grouped, spaced.trim_end().to_string())
}

pub fn code_expired(expiry_unix: u64, now_unix: u64) -> bool {
    now_unix > expiry_unix
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPayload {
    pub receipts: Vec<String>,
    pub draft: String,
    pub path: String,
    /// Present only when clipboard sync was explicitly opted in.
    pub clipboard: Option<String>,
}

impl SyncPayload {
    /// An already executed confirmation is not copied. Geometry is not a field.
    pub fn from_local(
        receipts: Vec<String>,
        draft: &str,
        path: &str,
        executed: bool,
        clipboard: Option<&str>,
    ) -> Self {
        Self {
            receipts,
            draft: if executed {
                String::new()
            } else {
                draft.to_string()
            },
            path: path.to_string(),
            clipboard: clipboard.and_then(nonempty_clipboard),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.clipboard.is_some() {
            out.extend_from_slice(b"w3sync2\n");
        } else {
            out.extend_from_slice(b"w3sync1\n");
        }
        out.extend_from_slice(self.path.as_bytes());
        out.extend_from_slice(b"\n");
        out.extend_from_slice(self.draft.as_bytes());
        out.extend_from_slice(b"\n");
        if let Some(clipboard) = &self.clipboard {
            out.extend_from_slice(clipboard.as_bytes());
            out.extend_from_slice(b"\n");
        }
        for line in &self.receipts {
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\n");
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        let version = lines.next()?;
        let path = lines.next()?.to_string();
        let draft = lines.next()?.to_string();
        let clipboard = if version == "w3sync2" {
            Some(lines.next()?.to_string()).and_then(|text| nonempty_clipboard(&text))
        } else if version == "w3sync1" {
            None
        } else {
            return None;
        };
        let receipts = lines
            .map(str::to_string)
            .filter(|line| !line.is_empty())
            .collect();
        Some(Self {
            receipts,
            draft,
            path,
            clipboard,
        })
    }
}

fn nonempty_clipboard(text: &str) -> Option<String> {
    let flat = text.replace(['\n', '\r'], " ");
    let trimmed = flat.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn seal(local: &DeviceKey, peer_public: &[u8; 32], payload: &SyncPayload) -> Option<Vec<u8>> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).ok()?;
    let plain = payload.encode();
    crate::hpke::seal(peer_public, &local.public_bytes(), &plain, seed)
}

pub fn open_sealed(
    local: &DeviceKey,
    peer_public: &[u8; 32],
    sealed: &[u8],
) -> Option<SyncPayload> {
    let plain = crate::hpke::open(&local.secret, peer_public, sealed)?;
    SyncPayload::decode(&plain)
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub name: String,
    pub fingerprint: String,
    pub public: [u8; 32],
    pub nonce: String,
    pub expiry_unix: u64,
    pub trusted: bool,
    /// libp2p peer id, empty until a pairing payload carries one.
    pub peer_id: String,
    pub ed25519: [u8; 32],
    pub link: w3cos_p2p::LinkMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Pending,
    ChooseSync,
    Sending,
    NoAnswer,
    Incoming,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncStep {
    Receipts,
    Drafts,
    Scene,
    Clipboard,
    Done,
}

/// One sync surface, asked on its own. None of them start enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncSurface {
    Receipts,
    Drafts,
    Scene,
    Clipboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerKind {
    Offer,
    Sending,
    NoAnswer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipSide {
    pub device: String,
    pub unix: u64,
    pub text: String,
}

struct Store {
    local: DeviceKey,
    name: String,
    peers: Vec<Peer>,
    phase: Phase,
    notice: String,
    incoming_path: String,
    incoming_draft: String,
    incoming_from: String,
    incoming_revealed: bool,
    sync_receipts: bool,
    sync_drafts: bool,
    sync_scene: bool,
    sync_clipboard: bool,
    scene_decided: bool,
    scene_gate: bool,
    sync_step: SyncStep,
    send_started: u64,
    clip_local: Option<ClipSide>,
    clip_remote: Option<ClipSide>,
    clip_until: u64,
    pending_clip_write: Option<String>,
    listener: bool,
}

impl Store {
    fn new() -> Self {
        let seed = load_or_create_seed();
        Self {
            local: DeviceKey::from_seed(seed),
            name: std::env::var("W3COS_DEVICE_NAME").unwrap_or_else(|_| "这台设备".to_string()),
            peers: Vec::new(),
            phase: Phase::Idle,
            notice: String::new(),
            incoming_path: String::new(),
            incoming_draft: String::new(),
            incoming_from: String::new(),
            incoming_revealed: false,
            sync_receipts: false,
            sync_drafts: false,
            sync_scene: false,
            sync_clipboard: false,
            scene_decided: false,
            scene_gate: false,
            sync_step: SyncStep::Done,
            send_started: 0,
            clip_local: None,
            clip_remote: None,
            clip_until: 0,
            pending_clip_write: None,
            listener: false,
        }
    }
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::new());
}

fn load_or_create_seed() -> [u8; 32] {
    if let Ok(raw) = std::env::var("W3COS_DEVICE_SEED")
        && let Some(seed) = parse_seed(&raw)
    {
        return seed;
    }
    let mut seed = [0u8; 32];
    let _ = getrandom::getrandom(&mut seed);
    seed
}

fn parse_seed(raw: &str) -> Option<[u8; 32]> {
    if raw.len() < 32 {
        return None;
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&raw.as_bytes()[..32]);
    Some(seed)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

pub fn local_name() -> String {
    STORE.with(|slot| slot.borrow().name.clone())
}

pub fn local_fingerprint() -> String {
    STORE.with(|slot| slot.borrow().local.fingerprint())
}

pub fn notice() -> String {
    STORE.with(|slot| slot.borrow().notice.clone())
}

pub fn peers() -> Vec<Peer> {
    STORE.with(|slot| slot.borrow().peers.clone())
}

pub fn pending_peer() -> Option<Peer> {
    STORE.with(|slot| {
        let store = slot.borrow();
        if store.phase != Phase::Pending {
            return None;
        }
        store.peers.iter().find(|peer| !peer.trusted).cloned()
    })
}

pub fn sending() -> bool {
    STORE.with(|slot| slot.borrow().phase == Phase::Sending)
}

pub fn banner_kind() -> Option<BannerKind> {
    STORE.with(|slot| {
        let store = slot.borrow();
        if !store.peers.iter().any(|peer| peer.trusted) {
            return None;
        }
        match store.phase {
            Phase::Idle => Some(BannerKind::Offer),
            Phase::Sending => Some(BannerKind::Sending),
            Phase::NoAnswer => Some(BannerKind::NoAnswer),
            _ => None,
        }
    })
}

pub fn show_banner() -> bool {
    banner_kind().is_some()
}

pub fn handoff_target_name() -> String {
    peers()
        .into_iter()
        .find(|peer| peer.trusted)
        .map(|peer| peer.name)
        .unwrap_or_default()
}

pub fn sync_prompt() -> Option<SyncSurface> {
    STORE.with(|slot| {
        let store = slot.borrow();
        if store.scene_gate && !store.scene_decided {
            return Some(SyncSurface::Scene);
        }
        if store.phase != Phase::ChooseSync {
            return None;
        }
        match store.sync_step {
            SyncStep::Receipts => Some(SyncSurface::Receipts),
            SyncStep::Drafts => Some(SyncSurface::Drafts),
            SyncStep::Scene => Some(SyncSurface::Scene),
            SyncStep::Clipboard => Some(SyncSurface::Clipboard),
            SyncStep::Done => None,
        }
    })
}

pub fn scene_gate_open() -> bool {
    STORE.with(|slot| {
        let store = slot.borrow();
        store.scene_gate && !store.scene_decided
    })
}

pub fn sync_scene_enabled() -> bool {
    STORE.with(|slot| slot.borrow().sync_scene)
}

pub fn scene_was_decided() -> bool {
    STORE.with(|slot| slot.borrow().scene_decided)
}

pub fn sync_receipts_enabled() -> bool {
    STORE.with(|slot| slot.borrow().sync_receipts)
}

pub fn sync_drafts_enabled() -> bool {
    STORE.with(|slot| slot.borrow().sync_drafts)
}

/// Continue may send when this returns true. Otherwise the scene sheet is up.
pub fn request_continue() -> bool {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if store.scene_decided {
            return true;
        }
        store.scene_gate = true;
        false
    })
}

pub fn allow_current() {
    advance_sync(true);
}

pub fn skip_current() {
    advance_sync(false);
}

fn advance_sync(allow: bool) {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if store.scene_gate && !store.scene_decided {
            store.sync_scene = allow;
            store.scene_decided = true;
            store.scene_gate = false;
            return;
        }
        if store.phase != Phase::ChooseSync {
            return;
        }
        match store.sync_step {
            SyncStep::Receipts => {
                store.sync_receipts = allow;
                store.sync_step = SyncStep::Drafts;
            }
            SyncStep::Drafts => {
                store.sync_drafts = allow;
                store.sync_step = SyncStep::Scene;
            }
            SyncStep::Scene => {
                store.sync_scene = allow;
                store.scene_decided = true;
                store.sync_step = SyncStep::Clipboard;
            }
            SyncStep::Clipboard => {
                store.sync_clipboard = allow;
                store.sync_step = SyncStep::Done;
                store.phase = Phase::Idle;
                store.notice.clear();
            }
            SyncStep::Done => {}
        }
    });
}

pub fn incoming() -> Option<(String, String, String)> {
    STORE.with(|slot| {
        let store = slot.borrow();
        if store.phase != Phase::Incoming {
            return None;
        }
        Some((
            store.incoming_from.clone(),
            store.incoming_path.clone(),
            store.incoming_draft.clone(),
        ))
    })
}

pub fn incoming_revealed() -> bool {
    STORE.with(|slot| {
        let store = slot.borrow();
        store.phase == Phase::Incoming && store.incoming_revealed
    })
}

pub fn reveal_incoming() {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if store.phase == Phase::Incoming {
            store.incoming_revealed = true;
        }
    });
}

pub fn clip_conflict() -> Option<(ClipSide, ClipSide)> {
    STORE.with(|slot| {
        let store = slot.borrow();
        if store.clip_until == 0 || now_unix() > store.clip_until {
            return None;
        }
        Some((store.clip_local.clone()?, store.clip_remote.clone()?))
    })
}

pub fn clip_deadline() -> u64 {
    STORE.with(|slot| slot.borrow().clip_until)
}

/// Records the choice and returns its text. The two copies stay until the deadline.
pub fn choose_clipboard(remote: bool) -> Option<String> {
    STORE.with(|slot| {
        let store = slot.borrow();
        if store.clip_until == 0 || now_unix() > store.clip_until {
            return None;
        }
        let side = if remote {
            store.clip_remote.as_ref()
        } else {
            store.clip_local.as_ref()
        };
        side.map(|side| side.text.clone())
    })
}

pub fn note_clock() -> bool {
    note_clock_at(now_unix())
}

pub fn note_clock_at(now: u64) -> bool {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        let mut changed = false;
        if store.phase == Phase::Sending
            && store.send_started > 0
            && now.saturating_sub(store.send_started) >= NO_ANSWER_AFTER
        {
            store.phase = Phase::NoAnswer;
            store.notice = "handoff-no-answer".to_string();
            changed = true;
        }
        if store.clip_until > 0 && now > store.clip_until {
            store.clip_local = None;
            store.clip_remote = None;
            store.clip_until = 0;
            changed = true;
        }
        changed
    })
}

pub fn note_announcement(packet: &str, now: u64) -> bool {
    let Some(parsed) = parse_announcement(packet) else {
        return false;
    };
    let ParsedAnnounce {
        name,
        fingerprint,
        public,
        nonce,
        expiry_unix,
        peer_id,
        ed25519,
    } = parsed;
    if code_expired(expiry_unix, now) {
        return false;
    }
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if fingerprint == store.local.fingerprint() {
            return false;
        }
        if let Some(existing) = store
            .peers
            .iter_mut()
            .find(|peer| peer.fingerprint == fingerprint)
        {
            existing.name = name;
            existing.public = public;
            existing.nonce = nonce;
            existing.expiry_unix = expiry_unix;
            if !peer_id.is_empty() {
                existing.peer_id = peer_id;
            }
            if ed25519 != [0; 32] {
                existing.ed25519 = ed25519;
            }
            return true;
        }
        store.peers.push(Peer {
            name,
            fingerprint,
            public,
            nonce,
            expiry_unix,
            trusted: false,
            peer_id,
            ed25519,
            link: w3cos_p2p::LinkMode::Offline,
        });
        if matches!(store.phase, Phase::Idle | Phase::Failed | Phase::NoAnswer) {
            store.phase = Phase::Pending;
        }
        true
    })
}

pub fn announcement(nonce: &str, expiry_unix: u64) -> String {
    let (peer_id, ed25519) = local_transport_card();
    STORE.with(|slot| {
        let store = slot.borrow();
        format!(
            "W3COS1\t{}\t{}\t{}\t{nonce}\t{expiry_unix}\t{peer_id}\t{ed25519}",
            store.name,
            store.local.fingerprint(),
            hex_key(&store.local.public_bytes()),
        )
    })
}

pub fn codes_differ() {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.peers.retain(|peer| peer.trusted);
        store.phase = Phase::Failed;
        store.notice = "pair-mismatch".to_string();
    });
}

pub fn codes_match() -> bool {
    let trusted = STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        let Some(peer) = store.peers.iter_mut().find(|peer| !peer.trusted) else {
            return None;
        };
        if code_expired(peer.expiry_unix, now_unix()) {
            store.phase = Phase::Failed;
            store.notice = "pair-expired".to_string();
            return None;
        }
        peer.trusted = true;
        let armed = peer.clone();
        store.phase = Phase::ChooseSync;
        store.sync_step = SyncStep::Receipts;
        store.sync_receipts = false;
        store.sync_drafts = false;
        store.sync_scene = false;
        store.sync_clipboard = false;
        store.scene_decided = false;
        store.scene_gate = false;
        store.notice.clear();
        Some(armed)
    });
    if let Some(peer) = trusted.as_ref() {
        arm_transport(peer);
    }
    trusted.is_some()
}

pub fn allow_receipt_sync() {
    STORE.with(|slot| slot.borrow_mut().sync_receipts = true);
}

pub fn allow_draft_sync() {
    STORE.with(|slot| slot.borrow_mut().sync_drafts = true);
}

/// Clipboard leaves the device only after this is called. It starts off.
pub fn allow_clipboard_sync() {
    STORE.with(|slot| slot.borrow_mut().sync_clipboard = true);
}

pub fn clipboard_sync_enabled() -> bool {
    STORE.with(|slot| slot.borrow().sync_clipboard)
}

pub fn hide_banner() {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if matches!(store.phase, Phase::Idle | Phase::NoAnswer | Phase::Failed) {
            store.phase = Phase::Failed;
            store.notice.clear();
        }
    });
}

pub fn cancel_send() {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if store.phase == Phase::Sending {
            store.phase = Phase::Idle;
            store.send_started = 0;
            store.notice.clear();
        }
    });
}

fn compose_payload(
    store: &Store,
    receipts: Vec<String>,
    draft: &str,
    path: &str,
    executed: bool,
    clipboard: Option<&str>,
) -> SyncPayload {
    // Geometry, pixels, locale, and the confirmation itself are not fields.
    SyncPayload::from_local(
        if store.sync_receipts {
            receipts
        } else {
            Vec::new()
        },
        if store.sync_drafts { draft } else { "" },
        if store.sync_scene { path } else { "" },
        executed,
        if store.sync_clipboard {
            clipboard
        } else {
            None
        },
    )
}

pub fn begin_send(
    receipts: Vec<String>,
    draft: &str,
    path: &str,
    executed: bool,
    clipboard: Option<&str>,
) -> Option<Vec<u8>> {
    let sealed = STORE.with(|slot| {
        let store = slot.borrow();
        let peer = store.peers.iter().find(|peer| peer.trusted)?.clone();
        let payload = compose_payload(&store, receipts, draft, path, executed, clipboard);
        seal(&store.local, &peer.public, &payload)
    })?;
    let sent = send_sealed(&sealed);
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.send_started = now_unix();
        if sent {
            store.phase = Phase::Sending;
            store.notice = "handoff-sending".to_string();
        } else {
            store.phase = Phase::NoAnswer;
            store.notice = "handoff-no-answer".to_string();
        }
    });
    Some(sealed)
}

fn accept_sealed(sealed: &[u8]) -> bool {
    let local_clip = read_system_clipboard();
    let peers = peers()
        .into_iter()
        .filter(|peer| peer.trusted)
        .collect::<Vec<_>>();
    for peer in peers {
        if receive_sealed(&peer.public, &peer.name, sealed, local_clip.as_deref()) {
            return true;
        }
    }
    false
}

pub fn note_ack(fingerprint: &str) -> bool {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if store.phase != Phase::Sending {
            return false;
        }
        let known = store
            .peers
            .iter()
            .any(|peer| peer.trusted && peer.fingerprint == fingerprint);
        if !known {
            return false;
        }
        store.phase = Phase::Failed;
        store.send_started = 0;
        store.notice.clear();
        true
    })
}

pub fn receive_sealed(
    peer_public: &[u8; 32],
    from: &str,
    sealed: &[u8],
    local_clipboard: Option<&str>,
) -> bool {
    let Some(payload) = STORE.with(|slot| {
        let store = slot.borrow();
        open_sealed(&store.local, peer_public, sealed)
    }) else {
        return false;
    };
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.incoming_from = from.to_string();
        store.incoming_path = payload.path;
        store.incoming_draft = payload.draft;
        store.incoming_revealed = false;
        store.phase = Phase::Incoming;
        store.notice = "handoff-incoming".to_string();
        settle_clipboard(&mut store, payload.clipboard, local_clipboard);
    });
    true
}

fn settle_clipboard(store: &mut Store, remote: Option<String>, local: Option<&str>) {
    let remote = remote.and_then(|text| nonempty_clipboard(&text));
    let local = local.and_then(nonempty_clipboard);
    match (local, remote) {
        (Some(local_text), Some(remote_text)) => {
            let now = now_unix();
            store.clip_local = Some(ClipSide {
                device: store.name.clone(),
                unix: now,
                text: local_text,
            });
            store.clip_remote = Some(ClipSide {
                device: store.incoming_from.clone(),
                unix: now,
                text: remote_text,
            });
            store.clip_until = now + CLIP_HOLD_SECS;
            store.pending_clip_write = None;
        }
        (None, Some(remote_text)) => {
            store.pending_clip_write = Some(remote_text);
        }
        _ => {}
    }
}

pub fn take_auto_clipboard() -> Option<String> {
    STORE.with(|slot| slot.borrow_mut().pending_clip_write.take())
}

pub fn local_public() -> [u8; 32] {
    STORE.with(|slot| slot.borrow().local.public_bytes())
}

pub fn demo_pending() {
    let peer = DeviceKey::from_seed([9; 32]);
    let packet = format!(
        "W3COS1\t手机\t{}\t{}\tdemo\t{}",
        peer.fingerprint(),
        hex_key(&peer.public_bytes()),
        now_unix() + CODE_TTL_SECS
    );
    let _ = note_announcement(&packet, now_unix());
}

pub fn demo_banner() {
    demo_pending();
    let _ = codes_match();
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.phase = Phase::Idle;
        store.sync_step = SyncStep::Done;
        store.scene_gate = false;
        store.notice.clear();
    });
}

pub fn demo_sending() {
    demo_banner();
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.phase = Phase::Sending;
        store.send_started = now_unix();
        store.notice = "handoff-sending".to_string();
    });
}

pub fn demo_no_answer() {
    demo_banner();
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.phase = Phase::NoAnswer;
        store.notice = "handoff-no-answer".to_string();
    });
}

pub fn demo_incoming() {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        let now = now_unix();
        store.incoming_from = "书桌".to_string();
        store.incoming_path = "/docs/notes.txt".to_string();
        store.incoming_draft = "把这段改成夜间模式".to_string();
        store.incoming_revealed = true;
        store.phase = Phase::Incoming;
        store.clip_local = Some(ClipSide {
            device: store.name.clone(),
            unix: now.saturating_sub(120),
            text: "手机上的一句".to_string(),
        });
        store.clip_remote = Some(ClipSide {
            device: "书桌".to_string(),
            unix: now.saturating_sub(30),
            text: "书桌上的一句".to_string(),
        });
        store.clip_until = now + CLIP_HOLD_SECS;
    });
}

pub fn dismiss_incoming() {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        store.phase = Phase::Idle;
        store.incoming_draft.clear();
        store.notice.clear();
    });
}

pub fn ensure_listener() {
    let start = STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        if store.listener {
            return false;
        }
        store.listener = true;
        true
    });
    if !start {
        return;
    }
    std::thread::spawn(|| {
        let port = std::env::var("W3COS_DEVICE_PORT")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(DISCOVERY_PORT);
        let Ok(socket) = UdpSocket::bind(("0.0.0.0", port)) else {
            return;
        };
        let _ = socket.set_broadcast(true);
        let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
        let mut buf = [0u8; 2048];
        let mut ticks = 0u32;
        loop {
            ticks = ticks.wrapping_add(1);
            if let Ok((size, _)) = socket.recv_from(&mut buf) {
                let bytes = &buf[..size];
                let changed = if bytes.starts_with(b"W3COS1") {
                    std::str::from_utf8(bytes).is_ok_and(|text| note_announcement(text, now_unix()))
                } else if let Some(sealed) = bytes.strip_prefix(b"W3COS2") {
                    let accepted = accept_sealed(sealed);
                    if accepted {
                        if let Some(text) = take_auto_clipboard() {
                            write_system_clipboard(&text);
                        }
                        send_ack();
                    }
                    accepted
                } else if let Some(rest) = bytes.strip_prefix(b"W3COS3\t") {
                    std::str::from_utf8(rest).is_ok_and(|fingerprint| note_ack(fingerprint.trim()))
                } else {
                    false
                };
                if changed {
                    w3cos_runtime::state::mark_dirty();
                    w3cos_runtime::uitest::poke_event_loop();
                }
            }
            if ticks % 5 == 0 {
                let packet = announcement("live", now_unix() + CODE_TTL_SECS);
                let _ = socket.send_to(packet.as_bytes(), ("255.255.255.255", port));
            }
        }
    });
    std::thread::spawn(mdns_loop);
    ensure_transport();
}

const MDNS_SERVICE: &str = "_w3cos._udp.local";

/// Unsolicited mDNS TXT for `_w3cos._udp.local`. The TXT is the UDP announcement.
pub fn mdns_announce(announcement: &str) -> Vec<u8> {
    let mut answer = Vec::new();
    write_dns_name(&mut answer, MDNS_SERVICE);
    answer.extend_from_slice(&16u16.to_be_bytes());
    answer.extend_from_slice(&1u16.to_be_bytes());
    answer.extend_from_slice(&90u32.to_be_bytes());
    let rdata = txt_rdata(announcement);
    answer.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    answer.extend_from_slice(&rdata);
    let mut packet = Vec::with_capacity(12 + answer.len());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0x8400u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&answer);
    packet
}

pub fn mdns_announcement_text(packet: &[u8]) -> Option<String> {
    if packet.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    let ancount = u16::from_be_bytes([packet[6], packet[7]]);
    let mut offset = 12usize;
    for _ in 0..qdcount {
        skip_dns_name(packet, &mut offset)?;
        offset = offset.checked_add(4)?;
    }
    for _ in 0..ancount {
        let name = read_dns_name(packet, &mut offset)?;
        if offset + 10 > packet.len() {
            return None;
        }
        let typ = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let rdlen = u16::from_be_bytes([packet[offset + 8], packet[offset + 9]]) as usize;
        offset += 10;
        if offset + rdlen > packet.len() {
            return None;
        }
        let rdata = &packet[offset..offset + rdlen];
        offset += rdlen;
        if name.eq_ignore_ascii_case(MDNS_SERVICE) && typ == 16 {
            return txt_text(rdata);
        }
    }
    None
}

fn mdns_loop() {
    use std::net::Ipv4Addr;
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 5353)) else {
        return;
    };
    let group = Ipv4Addr::new(224, 0, 0, 251);
    let _ = socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let mut buf = [0u8; 2048];
    let mut ticks = 0u32;
    loop {
        ticks = ticks.wrapping_add(1);
        if let Ok((size, _)) = socket.recv_from(&mut buf)
            && let Some(text) = mdns_announcement_text(&buf[..size])
            && note_announcement(&text, now_unix())
        {
            w3cos_runtime::state::mark_dirty();
            w3cos_runtime::uitest::poke_event_loop();
        }
        if ticks % 5 == 0 {
            let packet = mdns_announce(&announcement("live", now_unix() + CODE_TTL_SECS));
            let _ = socket.send_to(&packet, (group, 5353));
        }
    }
}

fn write_dns_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

fn txt_rdata(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + 255).min(bytes.len());
        let chunk = &bytes[offset..end];
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
        offset = end;
    }
    if out.is_empty() {
        out.push(0);
    }
    out
}

fn txt_text(rdata: &[u8]) -> Option<String> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < rdata.len() {
        let len = rdata[offset] as usize;
        offset += 1;
        if offset + len > rdata.len() {
            return None;
        }
        out.extend_from_slice(&rdata[offset..offset + len]);
        offset += len;
    }
    String::from_utf8(out).ok()
}

fn skip_dns_name(packet: &[u8], offset: &mut usize) -> Option<()> {
    read_dns_name(packet, offset).map(|_| ())
}

fn read_dns_name(packet: &[u8], offset: &mut usize) -> Option<String> {
    let mut labels = Vec::new();
    let mut cursor = *offset;
    let mut jumped = false;
    for _ in 0..16 {
        if cursor >= packet.len() {
            return None;
        }
        let len = packet[cursor];
        if len == 0 {
            if !jumped {
                *offset = cursor + 1;
            }
            return Some(labels.join("."));
        }
        if len & 0xc0 == 0xc0 {
            if cursor + 1 >= packet.len() {
                return None;
            }
            let pointer = (usize::from(len & 0x3f) << 8) | usize::from(packet[cursor + 1]);
            if !jumped {
                *offset = cursor + 2;
                jumped = true;
            }
            cursor = pointer;
            continue;
        }
        cursor += 1;
        let end = cursor + usize::from(len);
        if end > packet.len() {
            return None;
        }
        labels.push(std::str::from_utf8(&packet[cursor..end]).ok()?.to_string());
        cursor = end;
        if !jumped {
            *offset = cursor;
        }
    }
    None
}

fn read_system_clipboard() -> Option<String> {
    let output = std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    nonempty_clipboard(&String::from_utf8_lossy(&output.stdout))
}

pub fn publish_clipboard(text: &str) {
    write_system_clipboard(text);
}

fn write_system_clipboard(text: &str) {
    if let Ok(mut child) = std::process::Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        && let Some(mut stdin) = child.stdin.take()
    {
        use std::io::Write;
        let _ = stdin.write_all(text.as_bytes());
    }
}

fn send_sealed(bytes: &[u8]) -> bool {
    let mut packet = b"W3COS2".to_vec();
    packet.extend_from_slice(bytes);
    let udp = broadcast_packet(&packet);
    let p2p = send_over_transport(bytes);
    udp || p2p
}

fn send_ack() {
    let packet = format!("W3COS3\t{}", local_fingerprint());
    let _ = broadcast_packet(packet.as_bytes());
    let _ = send_over_transport(packet.as_bytes());
}

fn broadcast_packet(bytes: &[u8]) -> bool {
    let port = std::env::var("W3COS_DEVICE_PORT")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DISCOVERY_PORT);
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return false;
    };
    let _ = socket.set_broadcast(true);
    socket.send_to(bytes, ("255.255.255.255", port)).is_ok()
}

struct ParsedAnnounce {
    name: String,
    fingerprint: String,
    public: [u8; 32],
    nonce: String,
    expiry_unix: u64,
    peer_id: String,
    ed25519: [u8; 32],
}

fn parse_announcement(packet: &str) -> Option<ParsedAnnounce> {
    let mut parts = packet.split('\t');
    if parts.next() != Some("W3COS1") {
        return None;
    }
    let name = parts.next()?.to_string();
    let fingerprint = parts.next()?.to_string();
    let public = parse_hex_key(parts.next()?)?;
    let nonce = parts.next()?.to_string();
    let expiry_unix = parts.next()?.parse().ok()?;
    let peer_id = parts.next().unwrap_or("").trim().to_string();
    let ed25519 = parts.next().and_then(parse_hex_key).unwrap_or([0; 32]);
    Some(ParsedAnnounce {
        name,
        fingerprint,
        public,
        nonce,
        expiry_unix,
        peer_id,
        ed25519,
    })
}

fn parse_hex_key(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn local_seed() -> [u8; 32] {
    STORE.with(|slot| slot.borrow().local.secret)
}

fn local_transport_card() -> (String, String) {
    let identity = w3cos_p2p::DeviceIdentity::from_seed(local_seed());
    (
        identity.peer_id.to_string(),
        hex_key(&identity.ed25519_public),
    )
}

static TRANSPORT: std::sync::Mutex<Option<w3cos_p2p::Endpoint>> = std::sync::Mutex::new(None);

fn ensure_transport() {
    if std::env::var("W3COS_P2P").ok().as_deref() == Some("0") {
        return;
    }
    let mut slot = TRANSPORT.lock().expect("transport");
    if slot.is_some() {
        return;
    }
    let mut config = w3cos_p2p::NetConfig::from_env();
    config.name = local_name();
    if let Ok(endpoint) = w3cos_p2p::Endpoint::spawn(config, local_seed()) {
        endpoint.publish();
        *slot = Some(endpoint);
    }
}

fn arm_transport(peer: &Peer) {
    if peer.peer_id.is_empty() || peer.ed25519 == [0; 32] {
        return;
    }
    let Ok(id) = peer.peer_id.parse() else {
        return;
    };
    let slot = TRANSPORT.lock().expect("transport");
    let Some(endpoint) = slot.as_ref() else {
        return;
    };
    endpoint.track(id, &peer.fingerprint, peer.ed25519);
    endpoint.resolve(id, &peer.fingerprint, peer.ed25519);
    endpoint.publish();
}

fn send_over_transport(bytes: &[u8]) -> bool {
    let targets = peers()
        .into_iter()
        .filter(|peer| peer.trusted && !peer.peer_id.is_empty())
        .map(|peer| peer.peer_id)
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return false;
    }
    let slot = TRANSPORT.lock().expect("transport");
    let Some(endpoint) = slot.as_ref() else {
        return false;
    };
    let mut sent = false;
    for peer in targets {
        let Ok(id) = peer.parse() else {
            continue;
        };
        sent |= endpoint.send(id, bytes.to_vec());
    }
    sent
}

/// Apply swarm events on the UI thread. The listener thread does not touch
/// the thread-local pairing store.
pub fn poll_transport() -> bool {
    let drain = {
        let slot = TRANSPORT.lock().expect("transport");
        let Some(endpoint) = slot.as_ref() else {
            return false;
        };
        endpoint.drain()
    };
    let mut changed = false;
    for (peer, mode) in drain.changes {
        changed |= note_link(&peer, mode);
    }
    for bytes in drain.inbound {
        changed |= accept_transport_bytes(&bytes);
    }
    changed
}

fn accept_transport_bytes(bytes: &[u8]) -> bool {
    if let Some(rest) = bytes.strip_prefix(b"W3COS3\t") {
        return std::str::from_utf8(rest).is_ok_and(|fingerprint| note_ack(fingerprint.trim()));
    }
    let accepted = accept_sealed(bytes);
    if accepted {
        if let Some(text) = take_auto_clipboard() {
            write_system_clipboard(&text);
        }
        send_ack();
    }
    accepted
}

pub fn note_link(peer_id: &str, mode: w3cos_p2p::LinkMode) -> bool {
    STORE.with(|slot| {
        let mut store = slot.borrow_mut();
        let Some(peer) = store.peers.iter_mut().find(|peer| peer.peer_id == peer_id) else {
            return false;
        };
        if peer.link == mode {
            return false;
        }
        peer.link = mode;
        true
    })
}

/// The path a continue banner would use. Direct wins over a relay.
pub fn offer_link() -> w3cos_p2p::LinkMode {
    let trusted = peers()
        .into_iter()
        .filter(|peer| peer.trusted)
        .collect::<Vec<_>>();
    if trusted
        .iter()
        .any(|peer| peer.link == w3cos_p2p::LinkMode::Direct)
    {
        w3cos_p2p::LinkMode::Direct
    } else if trusted
        .iter()
        .any(|peer| peer.link == w3cos_p2p::LinkMode::Relay)
    {
        w3cos_p2p::LinkMode::Relay
    } else {
        w3cos_p2p::LinkMode::Offline
    }
}

pub fn import_pairing_payload(text: &str) -> bool {
    let text = text.trim();
    if !text.starts_with("W3COS1\t") {
        return false;
    }
    note_announcement(text, now_unix())
}

pub fn pairing_qr_svg() -> String {
    let payload = announcement("live", now_unix() + CODE_TTL_SECS);
    let Ok(code) = qrcode::QrCode::new(payload.as_bytes()) else {
        return String::new();
    };
    code.render::<qrcode::render::svg::Color>()
        .min_dimensions(132, 132)
        .build()
}

fn hex_key(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

#[cfg(test)]
pub fn reset_for_tests() {
    STORE.with(|slot| *slot.borrow_mut() = Store::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_code_matches_from_either_side_and_groups_digits() {
        let (left, spaced) = short_code("did:key:zaaa", "did:key:zbbb", "nonce");
        let (right, _) = short_code("did:key:zbbb", "did:key:zaaa", "nonce");
        assert_eq!(left, right);
        assert_eq!(left.len(), 7);
        assert!(left.as_bytes()[3] == b' ');
        assert_eq!(spaced.chars().filter(|ch| ch.is_ascii_digit()).count(), 6);
        assert!(spaced.contains(' '));
    }

    #[test]
    fn mismatch_does_not_trust_and_a_match_does() {
        reset_for_tests();
        let local = DeviceKey::from_seed([7; 32]);
        let peer = DeviceKey::from_seed([9; 32]);
        let packet = format!(
            "W3COS1\t手机\t{}\t{}\tnonce\t{}",
            peer.fingerprint(),
            hex_key(&peer.public_bytes()),
            now_unix() + 30
        );
        assert!(note_announcement(&packet, now_unix()));
        codes_differ();
        assert!(peers().iter().all(|item| !item.trusted));
        assert!(note_announcement(&packet, now_unix()));
        assert!(codes_match());
        assert!(peers().iter().any(|item| item.trusted));
        let _ = local;
    }

    #[test]
    fn sealed_payload_roundtrips_receipts_and_drops_an_executed_draft() {
        let local = DeviceKey::from_seed([1; 32]);
        let peer = DeviceKey::from_seed([2; 32]);
        let payload = SyncPayload::from_local(
            vec!["#1 allowed open".to_string()],
            "打开文件",
            "/docs/notes.txt",
            true,
            None,
        );
        assert!(payload.draft.is_empty());
        assert!(payload.clipboard.is_none());
        let encoded = String::from_utf8(payload.encode()).unwrap();
        assert!(!encoded.contains("window"));
        assert!(!encoded.contains("maximize"));
        let sealed = seal(&local, &peer.public_bytes(), &payload).unwrap();
        assert!(
            sealed
                .windows(b"allowed".len())
                .all(|window| window != b"allowed")
        );
        let opened = open_sealed(&peer, &local.public_bytes(), &sealed).unwrap();
        assert_eq!(opened.receipts, payload.receipts);
        assert_eq!(opened.path, "/docs/notes.txt");
        assert!(opened.draft.is_empty());
    }

    #[test]
    fn an_expired_announcement_is_ignored() {
        let peer = DeviceKey::from_seed([3; 32]);
        let packet = format!(
            "W3COS1\t书桌\t{}\t{}\tn\t10",
            peer.fingerprint(),
            hex_key(&peer.public_bytes())
        );
        assert!(!note_announcement(&packet, 11));
    }

    #[test]
    fn receiving_a_payload_does_not_mark_it_executed() {
        let local = DeviceKey::from_seed([4; 32]);
        let peer = DeviceKey::from_seed([5; 32]);
        let payload =
            SyncPayload::from_local(Vec::new(), "打开夜间模式", "/docs/a.txt", false, None);
        assert!(payload.clipboard.is_none());
        let sealed = seal(&peer, &local.public_bytes(), &payload).unwrap();
        // The store's key is random, so open with the explicit keys here.
        let opened = open_sealed(&local, &peer.public_bytes(), &sealed).unwrap();
        assert_eq!(opened.draft, "打开夜间模式");
        assert_ne!(opened.draft, "executed");
        assert!(opened.clipboard.is_none());
    }

    #[test]
    fn clipboard_is_absent_until_the_sender_opts_in() {
        let plain = SyncPayload::from_local(Vec::new(), "打开文件", "/docs/a.txt", false, None);
        let encoded = String::from_utf8(plain.encode()).unwrap();
        assert!(encoded.starts_with("w3sync1\n"));
        assert!(!encoded.contains("clip-secret"));
        let with = SyncPayload::from_local(
            Vec::new(),
            "打开文件",
            "/docs/a.txt",
            false,
            Some("  clip-secret \n"),
        );
        assert_eq!(with.clipboard.as_deref(), Some("clip-secret"));
        let encoded = with.encode();
        let text = String::from_utf8(encoded.clone()).unwrap();
        assert!(text.starts_with("w3sync2\n"));
        let opened = SyncPayload::decode(&encoded).unwrap();
        assert_eq!(opened.clipboard.as_deref(), Some("clip-secret"));
        assert_eq!(opened.draft, "打开文件");
        let blank = SyncPayload::from_local(Vec::new(), "d", "/p", false, Some("  \n"));
        assert!(blank.clipboard.is_none());
        assert!(
            String::from_utf8(blank.encode())
                .unwrap()
                .starts_with("w3sync1\n")
        );
        reset_for_tests();
        assert!(!clipboard_sync_enabled());
        allow_clipboard_sync();
        assert!(clipboard_sync_enabled());
    }

    #[test]
    fn mdns_txt_roundtrips_the_udp_announcement() {
        reset_for_tests();
        let peer = DeviceKey::from_seed([6; 32]);
        let text = format!(
            "W3COS1\t手机\t{}\t{}\tnonce\t{}",
            peer.fingerprint(),
            hex_key(&peer.public_bytes()),
            now_unix() + 30
        );
        let packet = mdns_announce(&text);
        let recovered = mdns_announcement_text(&packet).expect("txt");
        assert_eq!(recovered, text);
        assert!(note_announcement(&recovered, now_unix()));
        assert!(peers().iter().any(|item| item.name == "手机"));
    }

    #[test]
    fn a_code_expires_after_ninety_seconds() {
        let now = now_unix();
        assert!(!code_expired(now + CODE_TTL_SECS, now));
        assert!(!code_expired(now + CODE_TTL_SECS, now + CODE_TTL_SECS));
        assert!(code_expired(now + CODE_TTL_SECS, now + CODE_TTL_SECS + 1));
    }

    #[test]
    fn trust_asks_each_surface_and_leaves_scene_and_clipboard_off() {
        reset_for_tests();
        demo_pending();
        assert!(codes_match());
        assert!(!sync_receipts_enabled());
        assert!(!sync_drafts_enabled());
        assert!(!sync_scene_enabled());
        assert!(!clipboard_sync_enabled());
        assert_eq!(sync_prompt(), Some(SyncSurface::Receipts));
        allow_current();
        assert!(sync_receipts_enabled());
        assert_eq!(sync_prompt(), Some(SyncSurface::Drafts));
        skip_current();
        assert!(!sync_drafts_enabled());
        assert_eq!(sync_prompt(), Some(SyncSurface::Scene));
        skip_current();
        assert!(!sync_scene_enabled());
        assert!(scene_was_decided());
        assert_eq!(sync_prompt(), Some(SyncSurface::Clipboard));
        skip_current();
        assert!(!clipboard_sync_enabled());
        assert!(sync_prompt().is_none());
        assert_eq!(banner_kind(), Some(BannerKind::Offer));
    }

    #[test]
    fn scene_off_omits_the_path_and_the_payload_has_no_foreign_state() {
        reset_for_tests();
        demo_pending();
        assert!(codes_match());
        for _ in 0..4 {
            skip_current();
        }
        let store = STORE.with(|slot| {
            let store = slot.borrow();
            compose_payload(
                &store,
                vec!["#1 allowed".to_string()],
                "打开文件",
                "/docs/notes.txt",
                false,
                Some("clip-secret"),
            )
        });
        assert!(store.path.is_empty());
        assert!(store.draft.is_empty());
        assert!(store.clipboard.is_none());
        assert!(store.receipts.is_empty());
        let encoded = String::from_utf8(store.encode()).unwrap();
        for forbidden in [
            "window",
            "maximize",
            "locale",
            "zh-CN",
            "confirmed",
            "geometry",
            "clip-secret",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "{forbidden} leaked into {encoded}"
            );
        }
    }

    #[test]
    fn allowed_scene_sends_the_path_and_an_executed_draft_stays_home() {
        reset_for_tests();
        demo_pending();
        assert!(codes_match());
        allow_current();
        allow_current();
        allow_current();
        skip_current();
        assert!(sync_scene_enabled());
        assert!(sync_drafts_enabled());
        assert!(!clipboard_sync_enabled());
        let open = STORE.with(|slot| {
            let store = slot.borrow();
            compose_payload(
                &store,
                Vec::new(),
                "把这段改成夜间模式",
                "/docs/notes.txt",
                false,
                Some("clip"),
            )
        });
        assert_eq!(open.path, "/docs/notes.txt");
        assert_eq!(open.draft, "把这段改成夜间模式");
        assert!(open.clipboard.is_none());
        let executed = STORE.with(|slot| {
            let store = slot.borrow();
            compose_payload(
                &store,
                Vec::new(),
                "把这段改成夜间模式",
                "/docs/notes.txt",
                true,
                None,
            )
        });
        assert!(executed.draft.is_empty());
        let encoded = String::from_utf8(executed.encode()).unwrap();
        assert!(!encoded.contains("confirmed"));
        assert!(!encoded.contains("把这段改成夜间模式"));
    }

    #[test]
    fn both_clipboards_stay_choosable_for_ten_minutes() {
        reset_for_tests();
        let peer = DeviceKey::from_seed([8; 32]);
        let payload = SyncPayload {
            receipts: Vec::new(),
            draft: "把这段改成夜间模式".to_string(),
            path: "/docs/notes.txt".to_string(),
            clipboard: Some("书桌上的一句".to_string()),
        };
        let sealed = seal(&peer, &local_public(), &payload).unwrap();
        assert!(receive_sealed(
            &peer.public_bytes(),
            "书桌",
            &sealed,
            Some("手机上的一句"),
        ));
        assert!(!incoming_revealed());
        let (local, remote) = clip_conflict().expect("both copies");
        assert_eq!(local.text, "手机上的一句");
        assert_eq!(remote.device, "书桌");
        assert_eq!(remote.text, "书桌上的一句");
        let until = clip_deadline();
        assert!(until >= now_unix() + CLIP_HOLD_SECS - 1);
        assert_eq!(choose_clipboard(false).as_deref(), Some("手机上的一句"));
        assert!(
            clip_conflict().is_some(),
            "choosing does not drop the other copy"
        );
        assert!(!note_clock_at(until));
        assert!(clip_conflict().is_some());
        assert!(note_clock_at(until + 1));
        assert!(clip_conflict().is_none());
    }

    #[test]
    fn a_remote_clipboard_alone_does_not_open_the_conflict_sheet() {
        reset_for_tests();
        let peer = DeviceKey::from_seed([8; 32]);
        let payload = SyncPayload {
            receipts: Vec::new(),
            draft: "d".to_string(),
            path: "/p".to_string(),
            clipboard: Some("only-remote".to_string()),
        };
        let sealed = seal(&peer, &local_public(), &payload).unwrap();
        assert!(receive_sealed(&peer.public_bytes(), "书桌", &sealed, None));
        assert!(clip_conflict().is_none());
        assert_eq!(take_auto_clipboard().as_deref(), Some("only-remote"));
    }

    #[test]
    fn continue_asks_for_scene_once_and_a_missed_ack_is_no_answer() {
        reset_for_tests();
        demo_banner();
        assert!(!scene_was_decided());
        assert!(!request_continue());
        assert_eq!(sync_prompt(), Some(SyncSurface::Scene));
        skip_current();
        assert!(scene_was_decided());
        assert!(!sync_scene_enabled());
        assert!(request_continue());
        demo_sending();
        let started = STORE.with(|slot| slot.borrow().send_started);
        assert!(sending());
        assert!(!note_clock_at(started + NO_ANSWER_AFTER - 1));
        assert!(note_clock_at(started + NO_ANSWER_AFTER));
        assert_eq!(banner_kind(), Some(BannerKind::NoAnswer));
        cancel_send();
        assert!(sending() || banner_kind() == Some(BannerKind::NoAnswer));
        hide_banner();
        assert!(banner_kind().is_none());
    }

    #[test]
    fn cancel_during_send_returns_to_the_offer() {
        reset_for_tests();
        demo_sending();
        assert_eq!(banner_kind(), Some(BannerKind::Sending));
        cancel_send();
        assert!(!sending());
        assert_eq!(banner_kind(), Some(BannerKind::Offer));
    }

    #[test]
    fn pairing_payload_keeps_the_transport_identity_and_old_packets_parse() {
        reset_for_tests();
        let mine = announcement("live", now_unix() + 30);
        assert_eq!(mine.split('\t').count(), 8);
        assert!(!note_announcement(&mine, now_unix()));
        let remote_key = DeviceKey::from_seed([6; 32]);
        let remote = w3cos_p2p::DeviceIdentity::from_seed([6; 32]);
        let legacy = format!(
            "W3COS1\t手机\t{}\t{}\tlive\t{}",
            remote_key.fingerprint(),
            hex_key(&remote_key.public_bytes()),
            now_unix() + 30
        );
        assert!(import_pairing_payload(&legacy));
        let extended = format!(
            "{legacy}\t{}\t{}",
            remote.peer_id,
            hex_key(&remote.ed25519_public)
        );
        assert!(note_announcement(&extended, now_unix()));
        let peer = peers()
            .into_iter()
            .find(|peer| peer.name == "手机")
            .unwrap();
        assert_eq!(peer.peer_id, remote.peer_id.to_string());
        assert_eq!(peer.ed25519, remote.ed25519_public);
        assert_eq!(peer.link, w3cos_p2p::LinkMode::Offline);
        assert!(note_link(&peer.peer_id, w3cos_p2p::LinkMode::Direct));
        assert!(codes_match());
        assert_eq!(offer_link(), w3cos_p2p::LinkMode::Direct);
        assert!(!import_pairing_payload("not a code"));
        let svg = pairing_qr_svg();
        assert!(svg.contains("<svg"), "{svg}");
        assert!(!svg.contains("已连接"));
    }
}
