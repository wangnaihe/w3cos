//! P3 modality surface.
//!
//! Voice, handwriting, and screen OCR stay blocked on this device: there is
//! no local model, and `ctx.screen` is still refused. Clipboard text is the
//! one live path. It becomes a candidate only after the person asks, and the
//! hub does not keep the text.

use crate::{IntentPayload, Modality, Proposal, Scope, Session};

/// How many Unicode scalars a clipboard candidate may carry.
pub const CLIPBOARD_LIMIT: usize = 1024;

/// Why a P3 source cannot produce text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalityBlock {
    /// No on-device recognizer is installed. Nothing is recorded.
    NoLocalModel,
    /// Screen capture stays refused. No screenshot is taken.
    ScreenRefused,
    /// Secure fields bypass every capture modality.
    SecureField,
    /// The session has ended.
    NoSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalityState {
    /// The person can ask. Nothing is read until they do.
    Ready,
    Blocked(ModalityBlock),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModalitySurface {
    pub modality: Modality,
    pub state: ModalityState,
    /// Scope a future grant would need. Clipboard text stays on this device.
    pub scope: Option<Scope>,
}

/// What a clipboard read is allowed to become.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardDecision {
    Insert(String),
    Empty,
    TooLong,
    Refused(ModalityBlock),
}

/// Voice, handwriting, clipboard, then screen OCR. Order matches the roadmap.
pub fn p3_surface(session: &Session) -> [ModalitySurface; 4] {
    [
        ModalitySurface {
            modality: Modality::Voice,
            state: modality_state(session, Modality::Voice),
            scope: None,
        },
        ModalitySurface {
            modality: Modality::Handwriting,
            state: modality_state(session, Modality::Handwriting),
            scope: None,
        },
        ModalitySurface {
            modality: Modality::Clipboard,
            state: modality_state(session, Modality::Clipboard),
            scope: None,
        },
        ModalitySurface {
            modality: Modality::Camera,
            state: modality_state(session, Modality::Camera),
            scope: Some(Scope::Screen),
        },
    ]
}

pub fn modality_state(session: &Session, modality: Modality) -> ModalityState {
    if session.id.is_empty() {
        return ModalityState::Blocked(ModalityBlock::NoSession);
    }
    if session.purpose.is_secure() {
        return ModalityState::Blocked(ModalityBlock::SecureField);
    }
    match modality {
        // A text transcript can still be routed later. This device has no
        // recorder and no ASR, so the surface stays blocked.
        Modality::Voice | Modality::Handwriting => {
            ModalityState::Blocked(ModalityBlock::NoLocalModel)
        }
        Modality::Camera => ModalityState::Blocked(ModalityBlock::ScreenRefused),
        Modality::Clipboard => ModalityState::Ready,
        Modality::Key | Modality::Swipe | Modality::Gesture | Modality::Handoff => {
            ModalityState::Blocked(ModalityBlock::NoLocalModel)
        }
    }
}

pub fn clipboard_decision(session: &Session, text: &str) -> ClipboardDecision {
    if session.id.is_empty() {
        return ClipboardDecision::Refused(ModalityBlock::NoSession);
    }
    if session.purpose.is_secure() {
        return ClipboardDecision::Refused(ModalityBlock::SecureField);
    }
    let text = text.trim();
    if text.is_empty() {
        return ClipboardDecision::Empty;
    }
    if text.chars().count() > CLIPBOARD_LIMIT {
        return ClipboardDecision::TooLong;
    }
    ClipboardDecision::Insert(text.to_string())
}

/// Clipboard text is a candidate. Audio, ink, and camera payloads are dropped
/// before any engine sees them. `None` means the caller should route normally.
pub fn route_p3(
    session: &Session,
    modality: Modality,
    payload: &IntentPayload,
) -> Option<Vec<Proposal>> {
    match modality {
        Modality::Voice if matches!(payload, IntentPayload::Audio { .. }) => Some(Vec::new()),
        Modality::Handwriting | Modality::Camera => Some(Vec::new()),
        Modality::Clipboard => Some(clipboard_proposals(session, payload)),
        _ => None,
    }
}

fn clipboard_proposals(session: &Session, payload: &IntentPayload) -> Vec<Proposal> {
    let IntentPayload::Text { text } = payload else {
        return Vec::new();
    };
    let ClipboardDecision::Insert(text) = clipboard_decision(session, text) else {
        return Vec::new();
    };
    vec![Proposal::candidate("clip", text, 1.0, "clipboard")]
}
