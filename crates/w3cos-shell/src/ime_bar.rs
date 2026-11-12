//! Shell pinyin session.
//!
//! The spelling, committed base, and undo stack live here. `w3cos-ime` only
//! looks the spelling up. Secure fields and direct (EN) mode never call the
//! engine. L2 is not consulted: the lookup hub is constructed with L2 off.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_ime::{
    ClipboardDecision, ImeHub, InkModelError, InkPoint, InkResult, InkStroke, InputIntent,
    IntentHint, IntentPayload, LocalHandwriting, LocalScreenText, LocalSpeech, Modality,
    ModalityBlock, ModelError, NoLocalHandwriting, NoLocalScreenText, NoLocalSpeech,
    PaddleHandwriting, PaddleScreenText, PinyinEngine, Proposal, Purpose, ScreenImage,
    ScreenModelError, ScreenOff, ScreenRegion, ScreenResult, SpeechOff, SpeechStart, WhisperSpeech,
    clipboard_decision, default_handwriting_dir, default_screen_text_dir,
    download_handwriting_model, download_screen_text_model, download_speech_model, flush_speech,
    handwriting_model_ready, model_ready, poll_speech, press_speech, recognize_ink,
    recognize_screen, release_speech, screen_text_ready,
};
use w3cos_runtime::state::{self, ImeFieldNote};
use w3cos_std::Component;
use w3cos_std::color::Color;
use w3cos_std::style::*;

use super::{
    CHROME_Z, OP_IME_1, OP_IME_ACTION, OP_IME_CLIPBOARD, OP_IME_INK, OP_IME_INK_CLEAR, OP_IME_MODE,
    OP_IME_PAGE_NEXT, OP_IME_PAGE_PREV, OP_IME_SCREEN, OP_IME_SCREEN_NO, OP_IME_SCREEN_YES,
    OP_IME_VOICE, OP_SCRIM, OP_SOFTKEY, ink_token, invoke, line_strong,
};
use crate::locale;

/// Room left above the bottom edge for the phone keyboard that sits under
/// the candidate bar.
pub const PHONE_KEYBOARD_RESERVE: f32 = 240.0;
/// Key rows plus padding. The phone stack leaves this much room above the
/// home indicator so the absolute keyboard does not cover the intent field.
pub const KEYBOARD_HEIGHT: f32 = 218.0;
const BAR_H: f32 = 44.0;
const CHIP_H: f32 = 28.0;
const KEY_ROW: f32 = 48.0;
const KEY_GAP: f32 = 4.0;
const KEY_MARGIN: f32 = 8.0;
const KEY_BORDER: f32 = 1.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScreenPhase {
    Idle,
    Ask,
    Drag,
    Read,
}

struct Caret {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone)]
struct CommandOffer {
    utterance: String,
    summary: String,
}

struct Session {
    /// `Some(true)` forces pinyin. `None` follows the active locale.
    override_pinyin: Option<bool>,
    spelling: String,
    /// Segmented composition from the hub preedit, when one is open.
    preedit: String,
    base: String,
    role: String,
    index: usize,
    secure: bool,
    has_field: bool,
    undo: Vec<String>,
    caret: Caret,
    candidates: Vec<String>,
    actions: Vec<CommandOffer>,
    ime: Option<w3cos_ime::Session>,
    focus_gen: u64,
    page: usize,
    pending: Vec<FieldUpdate>,
    /// Microphone hold is open. False means nothing is being captured.
    voice_recording: bool,
    /// Latest local transcript, not yet committed.
    voice_preview: String,
    /// The handwriting pad is open. Strokes stay out of the lexicon until a pick.
    ink_open: bool,
    ink_strokes: Vec<InkStroke>,
    ink_candidates: Vec<String>,
    /// Ask, then one drag. Idle does not capture. Snippets stay until a pick.
    screen_phase: ScreenPhase,
    screen_candidates: Vec<String>,
    screen_x0: f32,
    screen_y0: f32,
    screen_x1: f32,
    screen_y1: f32,
    screen_dragging: bool,
    /// Frame generation already in the cache when this capture was asked for.
    screen_wait_gen: u64,
}

pub(crate) struct FieldUpdate {
    pub role: String,
    pub text: String,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            override_pinyin: None,
            spelling: String::new(),
            preedit: String::new(),
            base: String::new(),
            role: String::new(),
            index: 0,
            secure: false,
            has_field: false,
            undo: Vec::new(),
            caret: Caret {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            candidates: Vec::new(),
            actions: Vec::new(),
            ime: None,
            focus_gen: 0,
            page: 0,
            pending: Vec::new(),
            voice_recording: false,
            voice_preview: String::new(),
            ink_open: false,
            ink_strokes: Vec::new(),
            ink_candidates: Vec::new(),
            screen_phase: ScreenPhase::Idle,
            screen_candidates: Vec::new(),
            screen_x0: 0.0,
            screen_y0: 0.0,
            screen_x1: 0.0,
            screen_y1: 0.0,
            screen_dragging: false,
            screen_wait_gen: 0,
        }
    }
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
    static SPEECH: RefCell<Box<dyn LocalSpeech>> =
        RefCell::new(Box::new(WhisperSpeech::open_default()));
    static SPEECH_DIR: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
    static MODEL_CONSENT: Cell<bool> = const { Cell::new(false) };
    static HAND: RefCell<Box<dyn LocalHandwriting>> =
        RefCell::new(Box::new(PaddleHandwriting::open_default()));
    static HAND_DIR: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
    static INK_CONSENT: Cell<bool> = const { Cell::new(false) };
    static INK_TOAST: Cell<Option<&'static str>> = const { Cell::new(None) };
    static SCREEN: RefCell<Box<dyn LocalScreenText>> =
        RefCell::new(Box::new(PaddleScreenText::open_default()));
    static SCREEN_DIR: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
    static SCREEN_CONSENT: Cell<bool> = const { Cell::new(false) };
    static SCREEN_TOAST: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// 0 none, 1 allowed, 2 cancelled. The shell writes the receipt.
    static SCREEN_DECISION: Cell<u8> = const { Cell::new(0) };
    static SCREEN_PIXELS: RefCell<Option<ScreenImage>> = const { RefCell::new(None) };
}

fn with_session<R>(body: impl FnOnce(&mut Session) -> R) -> R {
    SESSION.with(|slot| body(&mut slot.borrow_mut()))
}

fn pinyin_mode(session: &mut Session) -> bool {
    session
        .override_pinyin
        .unwrap_or_else(|| locale::current().starts_with("zh"))
}

fn text_slots(session: &Session) -> usize {
    let size = page_size();
    if !session.screen_candidates.is_empty()
        || !session.ink_candidates.is_empty()
        || session.actions.is_empty()
    {
        size
    } else {
        size.saturating_sub(1).max(1)
    }
}

fn reopen_ime(session: &mut Session) {
    let purpose = Purpose::for_field(&session.role, session.secure);
    if session
        .ime
        .as_ref()
        .is_some_and(|ime| !ime.id.is_empty() && ime.purpose == purpose)
    {
        return;
    }
    if let Some(ime) = session.ime.as_mut() {
        ime.end();
    }
    session.focus_gen = session.focus_gen.saturating_add(1);
    let hub = ImeHub::with_l2(false);
    session.ime = Some(hub.begin_session(
        format!("shell-{}-{}", session.index, session.focus_gen),
        purpose,
        vec!["zh-Hans".into(), "en".into()],
    ));
}

fn wipe_ime(session: &mut Session) {
    stop_voice(session);
    close_ink(session);
    if let Some(ime) = session.ime.as_mut() {
        ime.end();
    }
    session.ime = None;
    session.spelling.clear();
    session.preedit.clear();
    session.base.clear();
    session.candidates.clear();
    session.actions.clear();
    session.undo.clear();
    session.page = 0;
}

fn queue(session: &mut Session, text: String) {
    if session.has_field {
        state::queue_ime_replacement(session.index, text.clone());
    }
    session.pending.push(FieldUpdate {
        role: session.role.clone(),
        text,
    });
    state::mark_dirty();
}

fn page_size() -> usize {
    let (width, _) = state::shell_viewport();
    if width < 600.0 { 4 } else { 5 }
}

fn refresh(session: &mut Session) {
    session.candidates.clear();
    session.actions.clear();
    session.preedit.clear();
    if session.secure || !session.has_field {
        return;
    }
    reopen_ime(session);
    let Some(ime) = session.ime.clone() else {
        return;
    };
    let composing = pinyin_mode(session) && !session.spelling.is_empty();
    let text = if composing {
        session.spelling.clone()
    } else if ime.purpose == Purpose::Command {
        session.base.clone()
    } else {
        String::new()
    };
    if text.is_empty() {
        return;
    }
    let hint = match ime.purpose {
        Purpose::Command => Some(IntentHint::Command),
        Purpose::Search => Some(IntentHint::Search),
        _ => Some(IntentHint::Insert),
    };
    let intent = InputIntent {
        modality: Modality::Key,
        payload: IntentPayload::Text { text },
        interim: composing,
        locale: ime.locales.clone(),
        hint,
    };
    let hub = ImeHub::with_l2(false);
    let mut engine = PinyinEngine::new();
    let proposals = if composing {
        hub.handle_intent(&ime, &intent, &mut [&mut engine])
    } else {
        hub.handle_intent(
            &ime,
            &intent,
            &mut [] as &mut [&mut dyn w3cos_ime::InputEngine],
        )
    };
    for proposal in proposals {
        match proposal {
            Proposal::Preedit { text, .. } => session.preedit = text,
            Proposal::Candidate { text, .. } => session.candidates.push(text),
            Proposal::Action {
                kind,
                summary,
                requires_confirm,
                ..
            } if requires_confirm => session.actions.push(CommandOffer {
                utterance: kind,
                summary,
            }),
            _ => {}
        }
    }
    if composing
        && !session.secure
        && let Some(ranked) = crate::lexicon::ranked(&session.spelling)
    {
        session.candidates = ranked;
    }
    let slots = text_slots(session);
    let pages = session.candidates.len().div_ceil(slots).max(1);
    if session.page >= pages {
        session.page = pages - 1;
    }
}

fn composed_field(session: &mut Session) -> String {
    if session.spelling.is_empty() || session.secure || !pinyin_mode(session) {
        return session.base.clone();
    }
    let suffix = if session.preedit.is_empty() {
        w3cos_ime::preedit_display(&session.spelling)
    } else {
        session.preedit.clone()
    };
    format!("{}{}", session.base, suffix)
}

fn commit_at(session: &mut Session, index: usize) -> bool {
    if session.spelling.is_empty() {
        return false;
    }
    let spelling = session.spelling.clone();
    let skipped: Vec<String> = session.candidates.iter().take(index).cloned().collect();
    let Some(chunk) = session.candidates.get(index).cloned().or_else(|| {
        if index == 0 {
            Some(session.spelling.clone())
        } else {
            None
        }
    }) else {
        return false;
    };
    if !session.secure {
        crate::lexicon::note_commit(&learning_scope(), false, &spelling, &chunk, &skipped);
    }
    session.base.push_str(&chunk);
    session.undo.push(chunk);
    session.spelling.clear();
    session.page = 0;
    sync_note_value(session);
    refresh(session);
    queue(session, session.base.clone());
    true
}

fn undo(session: &mut Session) -> bool {
    if !session.spelling.is_empty() {
        return false;
    }
    let Some(chunk) = session.undo.last().cloned() else {
        return false;
    };
    if !session.base.ends_with(&chunk) {
        return false;
    }
    session.undo.pop();
    session.base.truncate(session.base.len() - chunk.len());
    sync_note_value(session);
    refresh(session);
    queue(session, session.base.clone());
    true
}

fn adopt(session: &mut Session, note: &ImeFieldNote) {
    let switched = session.has_field
        && (note.index != session.index
            || note.role != session.role
            || note.secure != session.secure);
    if switched && !session.spelling.is_empty() {
        session.spelling.clear();
        session.preedit.clear();
        session.candidates.clear();
        session.actions.clear();
        session.page = 0;
        queue(session, session.base.clone());
    }
    if switched || note.secure {
        stop_voice(session);
        close_ink(session);
        abandon_screen(session);
    }
    if switched {
        if let Some(ime) = session.ime.as_mut() {
            ime.end();
        }
        session.ime = None;
        session.undo.clear();
        session.spelling.clear();
        session.preedit.clear();
        session.candidates.clear();
        session.actions.clear();
        session.page = 0;
    }
    let starting = session.spelling.is_empty();
    session.index = note.index;
    session.role = note.role.clone();
    session.secure = note.secure;
    session.caret = Caret {
        x: note.x,
        y: note.y,
        w: note.width,
        h: note.height,
    };
    session.has_field = true;
    if starting {
        session.base = note.value.clone();
    }
    if session.secure {
        session.spelling.clear();
        session.preedit.clear();
        session.candidates.clear();
        session.actions.clear();
        session.page = 0;
    }
    reopen_ime(session);
}

fn learning_scope() -> String {
    super::app_key(state::get_signal(super::APP_SIGNAL)).to_string()
}

fn is_letter(name: &str) -> bool {
    name.len() == 1 && name.as_bytes()[0].is_ascii_lowercase()
}

/// Consume a shell key. `true` means the runtime must not also insert it.
pub(crate) fn on_key(name: &str) -> bool {
    with_session(|session| {
        if name == "Mod+Space" {
            session.override_pinyin = Some(!pinyin_mode(session));
            let had_spelling = !session.spelling.is_empty();
            close_ink(session);
            abandon_screen(session);
            session.spelling.clear();
            session.page = 0;
            refresh(session);
            if had_spelling {
                queue(session, session.base.clone());
            } else {
                state::mark_dirty();
            }
            return true;
        }
        if let Some(note) = state::current_ime_field() {
            adopt(session, &note);
        }
        if session.ink_open && is_letter(name) {
            close_ink(session);
            state::mark_dirty();
        }
        if name == "Escape" {
            if session.screen_phase != ScreenPhase::Idle || !session.screen_candidates.is_empty() {
                abandon_screen(session);
                refresh(session);
                state::mark_dirty();
                return true;
            }
            if session.ink_open {
                close_ink(session);
                refresh(session);
                state::mark_dirty();
                return true;
            }
            if session.spelling.is_empty() {
                return false;
            }
            if !session.secure
                && let Some(top) = session.candidates.first().cloned()
            {
                let spelling = session.spelling.clone();
                crate::lexicon::note_reject(&learning_scope(), false, &spelling, &top);
            }
            session.spelling.clear();
            session.page = 0;
            refresh(session);
            queue(session, session.base.clone());
            return true;
        }
        if name == "Mod+Z" {
            return undo(session);
        }
        if session.ink_open && !session.ink_strokes.is_empty() && name == "Backspace" {
            session.ink_strokes.clear();
            session.ink_candidates.clear();
            session.page = 0;
            state::mark_dirty();
            return true;
        }
        if !session.screen_candidates.is_empty() && !session.secure {
            match name {
                "Space" | "Enter" => {
                    commit_screen_at(session, 0);
                    return true;
                }
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                    let visible = (name.as_bytes()[0] - b'1') as usize;
                    let slots = text_slots(session);
                    if visible < slots {
                        let _ = commit_screen_at(session, session.page * slots + visible);
                    }
                    return true;
                }
                _ => {}
            }
        }
        if !session.ink_candidates.is_empty() && !session.secure {
            match name {
                "Space" | "Enter" => {
                    commit_ink_at(session, 0);
                    return true;
                }
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                    let visible = (name.as_bytes()[0] - b'1') as usize;
                    let slots = text_slots(session);
                    if visible < slots {
                        let _ = commit_ink_at(session, session.page * slots + visible);
                    }
                    return true;
                }
                _ => {}
            }
        } else if session.ink_open && matches!(name, "Space" | "Enter") {
            note_ink_toast("ime-toast-ink-empty");
            return true;
        }
        if session.secure || !pinyin_mode(session) || !session.has_field {
            return false;
        }
        match name {
            "Backspace" => {
                if !session.spelling.is_empty() {
                    session.spelling.pop();
                    session.page = 0;
                    refresh(session);
                    let shown = composed_field(session);
                    queue(session, shown);
                    return true;
                }
                undo(session)
            }
            "Space" | "Enter" => {
                if session.spelling.is_empty() {
                    return false;
                }
                commit_at(session, 0);
                true
            }
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                if session.spelling.is_empty() {
                    return false;
                }
                let visible = (name.as_bytes()[0] - b'1') as usize;
                let slots = text_slots(session);
                if visible < slots {
                    let _ = commit_at(session, session.page * slots + visible);
                }
                true
            }
            letter if is_letter(letter) => {
                session.spelling.push_str(letter);
                session.page = 0;
                refresh(session);
                let shown = composed_field(session);
                queue(session, shown);
                true
            }
            _ => false,
        }
    })
}

pub(crate) fn toggle_mode() {
    let _ = on_key("Mod+Space");
}

pub(crate) fn commit_candidate(visible: usize) -> bool {
    with_session(|session| {
        if !session.screen_candidates.is_empty() {
            if session.secure {
                return false;
            }
            return commit_screen_at(session, session.page * text_slots(session) + visible);
        }
        if !session.ink_candidates.is_empty() {
            if session.secure {
                return false;
            }
            return commit_ink_at(session, session.page * text_slots(session) + visible);
        }
        if session.secure || !pinyin_mode(session) {
            return false;
        }
        commit_at(session, session.page * text_slots(session) + visible)
    })
}

pub(crate) fn page_by(delta: isize) -> bool {
    with_session(|session| {
        if !candidates_visible(session) {
            return false;
        }
        let len = if !session.screen_candidates.is_empty() {
            session.screen_candidates.len()
        } else if session.ink_candidates.is_empty() {
            session.candidates.len()
        } else {
            session.ink_candidates.len()
        };
        let pages = len.div_ceil(text_slots(session)).max(1);
        let next = (session.page as isize + delta).clamp(0, pages as isize - 1) as usize;
        if next == session.page {
            return false;
        }
        session.page = next;
        state::mark_dirty();
        true
    })
}

/// Segmented composition suffix for the focused field, when this role owns it.
pub(crate) fn preedit_suffix(role: &str) -> Option<String> {
    with_session(|session| {
        if session.role != role
            || session.spelling.is_empty()
            || session.secure
            || !pinyin_mode(session)
        {
            return None;
        }
        Some(if session.preedit.is_empty() {
            w3cos_ime::preedit_display(&session.spelling)
        } else {
            session.preedit.clone()
        })
    })
}

/// Commit the open preedit when a submit action reads this field.
pub(crate) fn commit_active(role: &str) -> Option<String> {
    with_session(|session| {
        if session.role != role || session.spelling.is_empty() || session.secure {
            return None;
        }
        if !commit_at(session, 0) {
            return None;
        }
        Some(session.base.clone())
    })
}

pub(crate) fn take_field_updates() -> Vec<FieldUpdate> {
    with_session(|session| std::mem::take(&mut session.pending))
}

#[cfg(test)]
pub(crate) fn composed_text() -> String {
    with_session(|session| {
        if session.voice_recording && !session.voice_preview.is_empty() {
            format!("{}{}", session.base, session.voice_preview)
        } else {
            format!("{}{}", session.base, session.spelling)
        }
    })
}

pub(crate) fn candidate_origin(width: f32, height: f32) -> (f32, f32) {
    with_session(|session| place(width, height, &session.caret))
}

fn bar_width(width: f32) -> f32 {
    (width - 16.0).clamp(160.0, 560.0)
}

fn place(width: f32, height: f32, caret: &Caret) -> (f32, f32) {
    if width < 600.0 {
        let y = (height - PHONE_KEYBOARD_RESERVE - 8.0 - BAR_H).max(8.0);
        return (8.0, y);
    }
    let bar_w = bar_width(width);
    let intent_top = super::intent_block_top(height);
    let ceiling = (intent_top - 8.0 - BAR_H).max(8.0);
    let centered_x = ((width - bar_w) / 2.0).max(8.0);
    let near_intent = caret.h <= 1.0 || caret.y + caret.h > intent_top - 8.0;
    if near_intent {
        return (centered_x, ceiling);
    }
    let below = caret.y + caret.h + 6.0;
    let y = if below + BAR_H <= intent_top - 8.0 {
        below
    } else {
        (caret.y - BAR_H - 6.0).max(8.0).min(ceiling)
    };
    let x = caret.x.max(8.0).min((width - bar_w - 8.0).max(8.0));
    (x, y.min(ceiling))
}

/// Drop an uncommitted spelling when focus has left every text field.
pub(crate) fn sync_field_presence() {
    if state::current_ime_field().is_some() {
        return;
    }
    with_session(|session| {
        wipe_ime(session);
        session.has_field = false;
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardGate {
    Read,
    Secure,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardTap {
    Wrote,
    Empty,
    TooLong,
    Secure,
    Closed,
}

impl ClipboardTap {
    pub(crate) fn toast_key(self) -> &'static str {
        match self {
            ClipboardTap::Wrote => "ime-toast-clip-ok",
            ClipboardTap::Empty => "ime-toast-clip-empty",
            ClipboardTap::TooLong => "ime-toast-clip-long",
            ClipboardTap::Secure => "ime-toast-clip-secure",
            ClipboardTap::Closed => "ime-toast-clip-closed",
        }
    }

    pub(crate) fn wrote(self) -> bool {
        matches!(self, ClipboardTap::Wrote)
    }
}

/// Whether a clipboard tap may read this device. Secure and closed sessions
/// return before the shell touches the clipboard.
pub(crate) fn local_clipboard_gate() -> ClipboardGate {
    with_session(|session| {
        adopt_for_capture(session);
        gate_of(session)
    })
}

/// Insert clipboard text the shell already read. This does not read, does not
/// learn a personal word, and does not send the text to another device.
pub(crate) fn accept_local_clipboard(text: Option<&str>) -> ClipboardTap {
    with_session(|session| {
        adopt_for_capture(session);
        let Some(ime) = session.ime.clone() else {
            return ClipboardTap::Closed;
        };
        match clipboard_decision(&ime, text.unwrap_or("")) {
            ClipboardDecision::Insert(chunk) => {
                session.spelling.clear();
                session.preedit.clear();
                session.page = 0;
                session.base.push_str(&chunk);
                session.undo.push(chunk);
                sync_note_value(session);
                refresh(session);
                queue(session, session.base.clone());
                ClipboardTap::Wrote
            }
            ClipboardDecision::Empty => ClipboardTap::Empty,
            ClipboardDecision::TooLong => ClipboardTap::TooLong,
            ClipboardDecision::Refused(ModalityBlock::SecureField) => ClipboardTap::Secure,
            ClipboardDecision::Refused(_) => ClipboardTap::Closed,
        }
    })
}

/// Keep text the session already holds for this field. A later note can still
/// be the empty string from focus, and replacing `base` would drop a commit.
fn adopt_for_capture(session: &mut Session) {
    let Some(note) = state::current_ime_field() else {
        return;
    };
    let same = session.has_field
        && note.index == session.index
        && note.role == session.role
        && note.secure == session.secure;
    if !same || Purpose::for_field(&note.role, note.secure).is_secure() {
        adopt(session, &note);
        return;
    }
    let base = session.base.clone();
    let undo = session.undo.clone();
    let spelling = session.spelling.clone();
    adopt(session, &note);
    session.base = base;
    session.undo = undo;
    session.spelling = spelling;
}

fn sync_note_value(session: &Session) {
    let Some(mut note) = state::current_ime_field() else {
        return;
    };
    if note.index != session.index || note.role != session.role {
        return;
    }
    note.value = session.base.clone();
    state::note_ime_field(note);
}

fn close_microphone(session: &mut Session) {
    let was = session.voice_recording;
    session.voice_recording = false;
    SPEECH.with(|slot| release_speech(&mut **slot.borrow_mut(), was));
    state::note_shell_poll_soon(false);
}

fn stop_voice(session: &mut Session) {
    close_microphone(session);
    session.voice_preview.clear();
}

/// The bundled engine is off. A plugged-in local engine with no network can arm.
pub(crate) fn voice_engine_ready() -> bool {
    SPEECH.with(|slot| {
        let manifest = slot.borrow().manifest();
        manifest.available && !manifest.network
    })
}

pub(crate) fn voice_recording() -> bool {
    with_session(|session| session.voice_recording)
}

/// Short tap explains the mic. It does not start a capture.
pub(crate) fn voice_tap_key() -> &'static str {
    if voice_engine_ready() {
        "ime-toast-voice-hold"
    } else if model_ready(&speech_dir()) {
        "ime-toast-voice-mic"
    } else {
        "ime-toast-voice-offer"
    }
}

pub(crate) fn voice_off_key(reason: SpeechOff) -> &'static str {
    match reason {
        SpeechOff::NoLocalModel => "ime-toast-voice",
        SpeechOff::NoMicrophone => "ime-toast-voice-mic",
        SpeechOff::NetworkRefused => "ime-toast-voice-net",
        SpeechOff::SecureField => "ime-toast-voice-secure",
        SpeechOff::NoSession => "ime-toast-voice-closed",
    }
}

fn speech_dir() -> std::path::PathBuf {
    SPEECH_DIR.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(w3cos_ime::default_speech_dir)
    })
}

pub(crate) fn speech_model_status_key() -> &'static str {
    if model_ready(&speech_dir()) {
        "ime-voice-model-ready"
    } else {
        "ime-voice-model-off"
    }
}

/// `None` once the verified model is already on disk. The button never downloads then.
pub(crate) fn speech_model_button_key() -> Option<&'static str> {
    if model_ready(&speech_dir()) {
        None
    } else if MODEL_CONSENT.with(Cell::get) {
        Some("ime-voice-model-confirm")
    } else {
        Some("ime-voice-model-get")
    }
}

/// First press only arms consent. The second press downloads. Recognition does not call this.
pub(crate) fn confirm_speech_model() -> &'static str {
    if model_ready(&speech_dir()) {
        MODEL_CONSENT.with(|flag| flag.set(false));
        return "ime-toast-voice-saved";
    }
    if !MODEL_CONSENT.with(Cell::get) {
        MODEL_CONSENT.with(|flag| flag.set(true));
        return "ime-toast-voice-confirm";
    }
    MODEL_CONSENT.with(|flag| flag.set(false));
    match download_speech_model(&speech_dir(), true) {
        Ok(_) => "ime-toast-voice-saved",
        Err(ModelError::ChecksumMismatch { .. }) => "ime-toast-voice-checksum",
        Err(ModelError::ConsentRequired) => "ime-toast-voice-confirm",
        Err(_) => "ime-toast-voice-download-fail",
    }
}

/// Pointer down on the microphone. Off states return before any capture.
pub(crate) fn voice_press() -> SpeechStart {
    with_session(|session| {
        adopt_for_capture(session);
        let Some(ime) = session.ime.clone() else {
            return SpeechStart::Off(SpeechOff::NoSession);
        };
        if ime.id.is_empty() {
            return SpeechStart::Off(SpeechOff::NoSession);
        }
        let start = SPEECH.with(|slot| press_speech(&mut **slot.borrow_mut(), &ime));
        if matches!(start, SpeechStart::Listening) {
            close_ink(session);
            abandon_screen(session);
            session.spelling.clear();
            session.preedit.clear();
            session.page = 0;
            session.voice_preview.clear();
            session.voice_recording = true;
            state::note_shell_poll_soon(true);
            state::mark_dirty();
        } else {
            session.voice_recording = false;
            state::note_shell_poll_soon(false);
        }
        start
    })
}

/// Pull one local transcript into the field preview. No-op when not recording.
pub(crate) fn poll_voice() -> bool {
    with_session(|session| {
        if !session.voice_recording {
            return false;
        }
        let Some(chunk) = SPEECH.with(|slot| poll_speech(&mut **slot.borrow_mut(), true)) else {
            return false;
        };
        if chunk.text == session.voice_preview {
            return false;
        }
        session.voice_preview = chunk.text;
        queue(
            session,
            format!("{}{}", session.base, session.voice_preview),
        );
        true
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoiceRelease {
    Wrote,
    Empty,
    Idle,
}

/// Pointer up. Commits the latest local transcript once, then closes the mic.
pub(crate) fn voice_release() -> VoiceRelease {
    with_session(|session| {
        let was = session.voice_recording;
        if was && let Some(chunk) = SPEECH.with(|slot| flush_speech(&mut **slot.borrow_mut(), true))
        {
            session.voice_preview = chunk.text;
        }
        let text = std::mem::take(&mut session.voice_preview);
        close_microphone(session);
        if !was {
            return VoiceRelease::Idle;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            queue(session, session.base.clone());
            return VoiceRelease::Empty;
        }
        session.spelling.clear();
        session.preedit.clear();
        session.page = 0;
        session.base.push_str(&text);
        session.undo.push(text);
        sync_note_value(session);
        refresh(session);
        queue(session, session.base.clone());
        VoiceRelease::Wrote
    })
}

#[cfg(test)]
pub(crate) fn install_speech_for_tests(engine: Box<dyn LocalSpeech>) {
    SPEECH.with(|slot| {
        release_speech(&mut **slot.borrow_mut(), true);
        *slot.borrow_mut() = engine;
    });
}

#[cfg(test)]
pub(crate) fn speech_capturing() -> bool {
    SPEECH.with(|slot| slot.borrow().capturing())
}

fn gate_of(session: &Session) -> ClipboardGate {
    let Some(ime) = session.ime.as_ref() else {
        return ClipboardGate::Closed;
    };
    if ime.id.is_empty() {
        return ClipboardGate::Closed;
    }
    if ime.purpose.is_secure() {
        ClipboardGate::Secure
    } else {
        ClipboardGate::Read
    }
}

/// Utterance for the visible command chip. The shell proposes it; it does not run it.
pub(crate) fn command_utterance() -> Option<String> {
    with_session(|session| session.actions.first().map(|offer| offer.utterance.clone()))
}

#[cfg(test)]
pub(crate) fn focused_purpose() -> Option<Purpose> {
    with_session(|session| session.ime.as_ref().map(|ime| ime.purpose))
}

#[cfg(test)]
pub(crate) fn focused_grants_empty() -> bool {
    with_session(|session| {
        session
            .ime
            .as_ref()
            .is_some_and(|ime| ime.granted.is_empty())
    })
}

pub(crate) fn keyboard_open() -> bool {
    let (width, _) = state::shell_viewport();
    width < 600.0 && state::current_ime_field().is_some()
}

pub(crate) enum SoftOutcome {
    None,
    Edit,
    Submit { role: String, text: String },
}

/// A phone-keyboard key. Letters in direct or secure mode are inserted here
/// because there is no runtime key event to fall back on.
pub(crate) fn on_soft_key(name: &str) -> SoftOutcome {
    if on_key(name) {
        return SoftOutcome::Edit;
    }
    with_session(|session| {
        if !session.has_field {
            return SoftOutcome::None;
        }
        match name {
            "Backspace" => {
                let mut chars: Vec<char> = session.base.chars().collect();
                if chars.pop().is_some() {
                    session.base = chars.into_iter().collect();
                    refresh(session);
                    queue(session, session.base.clone());
                }
                SoftOutcome::Edit
            }
            "Enter" => SoftOutcome::Submit {
                role: session.role.clone(),
                text: session.base.clone(),
            },
            "Space" => {
                session.base.push(' ');
                refresh(session);
                queue(session, session.base.clone());
                SoftOutcome::Edit
            }
            other if other.chars().count() == 1 && !other.chars().any(|ch| ch.is_control()) => {
                session.base.push_str(other);
                refresh(session);
                queue(session, session.base.clone());
                SoftOutcome::Edit
            }
            _ => SoftOutcome::None,
        }
    })
}

fn candidates_visible(session: &mut Session) -> bool {
    if session.secure || !session.has_field {
        return false;
    }
    if !session.screen_candidates.is_empty() || !session.ink_candidates.is_empty() {
        return true;
    }
    let composing = pinyin_mode(session) && !session.spelling.is_empty();
    composing || !session.actions.is_empty()
}

fn candidate_children(show_preedit: bool) -> Vec<Component> {
    let (spelling, preedit, candidates, ink, screen, page, actions) = with_session(|session| {
        (
            session.spelling.clone(),
            session.preedit.clone(),
            session.candidates.clone(),
            session.ink_candidates.clone(),
            session.screen_candidates.clone(),
            session.page,
            session.actions.clone(),
        )
    });
    if !screen.is_empty() {
        return ink_candidate_children(&screen, page);
    }
    if !ink.is_empty() {
        return ink_candidate_children(&ink, page);
    }
    let size = if actions.is_empty() {
        page_size()
    } else {
        page_size().saturating_sub(1).max(1)
    };
    let pages = candidates.len().div_ceil(size).max(1);
    let page = page.min(pages - 1);
    let mut children = vec![mode_button()];
    if show_preedit && !spelling.is_empty() {
        let shown = if preedit.is_empty() {
            w3cos_ime::preedit_display(&spelling)
        } else {
            preedit
        };
        // An auto-sized text item in this fixed-height row collapses, which
        // previously clipped `nihao` down to `nih`. Reserve the full spelling.
        let width = shown
            .chars()
            .map(|ch| if ch.is_ascii() { 9.0 } else { 14.0 })
            .sum::<f32>()
            .max(24.0);
        children.push(Component::text(
            locale::t_args("ime-preedit", &[("spelling", &shown)]),
            Style {
                width: Dimension::Px(width),
                min_width: Dimension::Px(width),
                flex_shrink: 0.0,
                font_size: 13.0,
                font_weight: 650,
                color: ink_token(),
                white_space: WhiteSpace::NoWrap,
                ..Style::default()
            },
        ));
    }
    if page > 0 {
        children.push(candidate_button(
            locale::t("ime-page-prev"),
            OP_IME_PAGE_PREV,
        ));
    }
    for (index, text) in candidates.iter().skip(page * size).take(size).enumerate() {
        let n = (index + 1).to_string();
        let label = locale::t_args("ime-candidate", &[("n", &n), ("text", text)]);
        children.push(candidate_button(label, OP_IME_1 + index as u8));
    }
    if page + 1 < pages {
        children.push(candidate_button(
            locale::t("ime-page-next"),
            OP_IME_PAGE_NEXT,
        ));
    }
    if let Some(offer) = actions.first() {
        children.push(action_button(offer));
    }
    children
}

pub(crate) fn candidate_layer() -> Option<Component> {
    let (width, _) = state::shell_viewport();
    if width < 600.0 {
        return None;
    }
    if !with_session(candidates_visible) {
        return None;
    }
    let (width, height) = state::shell_viewport();
    let (x, y) = candidate_origin(width, height);
    let bar_w = bar_width(width);
    let mut bar = Component::row(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(x),
            top: Dimension::Px(y),
            width: Dimension::Px(bar_w),
            height: Dimension::Px(BAR_H),
            gap: 4.0,
            padding: Edges::xy(6.0, 0.0),
            flex_wrap: FlexWrap::NoWrap,
            overflow: Overflow::Hidden,
            align_items: AlignItems::Center,
            background: Color::from_hex("#162433"),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 12.0,
            box_sizing: BoxSizing::BorderBox,
            z_index: CHROME_Z + 5,
            ..Style::default()
        },
        candidate_children(true),
    );
    bar.style.z_index = CHROME_Z + 5;
    Some(bar)
}

fn field_blocks_capture() -> bool {
    let Some(note) = state::current_ime_field() else {
        return true;
    };
    Purpose::for_field(&note.role, note.secure).is_secure()
}

/// Phone row above the keyboard. The microphone is a hold target. It stays
/// off until a local speech engine is plugged in. Clipboard still reads only
/// after a tap.
pub(crate) fn phone_smart_bar() -> Option<Component> {
    if !keyboard_open() || field_blocks_capture() {
        return None;
    }
    Some(Component::row(smart_bar_style(false), modality_children()))
}

/// Desktop row near the caret when a field is focused and no candidates are open.
pub(crate) fn modality_layer() -> Option<Component> {
    let (width, height) = state::shell_viewport();
    if width < 600.0 || field_blocks_capture() || with_session(candidates_visible) {
        return None;
    }
    let (x, y) = candidate_origin(width, height);
    let mut bar = Component::row(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(x),
            top: Dimension::Px(y),
            width: Dimension::Px(bar_width(width)),
            z_index: CHROME_Z + 4,
            ..smart_bar_style(true)
        },
        modality_children(),
    );
    bar.style.z_index = CHROME_Z + 4;
    Some(bar)
}

fn smart_bar_style(desktop: bool) -> Style {
    Style {
        flex_shrink: 0.0,
        height: Dimension::Px(BAR_H),
        gap: 4.0,
        margin: if desktop {
            Edges::ZERO
        } else {
            Edges::xy(8.0, 4.0)
        },
        padding: Edges::xy(6.0, 0.0),
        flex_wrap: FlexWrap::NoWrap,
        overflow: Overflow::Hidden,
        align_items: AlignItems::Center,
        background: Color::from_hex("#162433"),
        border_width: 1.0,
        border_color: line_strong(),
        border_radius: 12.0,
        box_sizing: BoxSizing::BorderBox,
        ..Style::default()
    }
}

fn modality_children() -> Vec<Component> {
    let recording = voice_recording();
    let ready = voice_engine_ready();
    let voice_key = if recording {
        "ime-voice-live"
    } else if ready {
        "ime-voice-ready"
    } else {
        "ime-voice"
    };
    vec![
        modality_button(voice_key, OP_IME_VOICE, ready || recording, true),
        modality_button(ink_chip_key(), OP_IME_INK, ink_chip_ready(), false),
        modality_button("ime-clip", OP_IME_CLIPBOARD, true, false),
        modality_button(screen_chip_key(), OP_IME_SCREEN, screen_chip_ready(), false),
    ]
}

fn modality_button(key: &str, code: u8, ready: bool, hold: bool) -> Component {
    let mut style = Style {
        background: Color::from_hex(if ready { "#14362c" } else { "#243444" }),
        border_width: 1.0,
        border_color: Color::from_hex(if ready { "#1c8f62" } else { "#3d5570" }),
        border_radius: 8.0,
        font_size: 13.0,
        font_weight: 650,
        color: Color::from_hex(if ready { "#e9fff5" } else { "#c5d0dc" }),
        padding: Edges::xy(8.0, 0.0),
        height: Dimension::Px(CHIP_H),
        min_height: Dimension::Px(CHIP_H),
        flex_shrink: 0.0,
        flex_grow: 0.0,
        box_sizing: BoxSizing::BorderBox,
        white_space: WhiteSpace::NoWrap,
        ..Style::default()
    };
    keep_ime(&mut style);
    if hold {
        let mut props = style.custom_properties.take().unwrap_or_default();
        props.insert("--w3cos-hold".to_string(), code.to_string());
        style.custom_properties = Some(props);
    }
    Component::button_with_click(locale::t(key), style, invoke(code))
}

/// In-flow candidate row for the phone stack, just above the keyboard.
pub(crate) fn phone_candidate_strip() -> Option<Component> {
    if !keyboard_open() || !with_session(candidates_visible) {
        return None;
    }
    Some(Component::row(
        Style {
            flex_shrink: 0.0,
            height: Dimension::Px(BAR_H),
            gap: 4.0,
            margin: Edges::xy(8.0, 4.0),
            padding: Edges::xy(6.0, 0.0),
            flex_wrap: FlexWrap::NoWrap,
            overflow: Overflow::Hidden,
            align_items: AlignItems::Center,
            background: Color::from_hex("#162433"),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 12.0,
            ..Style::default()
        },
        candidate_children(false),
    ))
}

fn keep_ime(style: &mut Style) {
    let mut props = style.custom_properties.take().unwrap_or_default();
    props.insert("--w3cos-ime-keep".to_string(), "1".to_string());
    style.custom_properties = Some(props);
}

fn keyboard_inner(viewport: f32) -> f32 {
    (viewport - KEY_MARGIN * 2.0 - KEY_BORDER * 2.0).max(0.0)
}

fn even_key(inner: f32, count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let gaps = KEY_GAP * (count.saturating_sub(1) as f32);
    let raw = (inner - gaps) / count as f32;
    (raw * 10.0).floor() / 10.0
}

fn soft_key(label: String, key: &str, width: f32, special: bool) -> Component {
    let mut style = Style {
        width: Dimension::Px(width),
        min_width: Dimension::Px(width),
        max_width: Dimension::Px(width),
        flex_grow: 0.0,
        flex_shrink: 0.0,
        height: Dimension::Px(KEY_ROW),
        min_height: Dimension::Px(KEY_ROW),
        background: Color::from_hex(if special { "#1b2c3e" } else { "#24384a" }),
        border_radius: 8.0,
        font_size: 16.0,
        font_weight: 650,
        color: Color::from_hex("#f4f7fb"),
        padding: Edges::ZERO,
        box_sizing: BoxSizing::BorderBox,
        overflow: Overflow::Hidden,
        ..Style::default()
    };
    style.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([
        ("--w3cos-softkey".to_string(), key.to_string()),
        ("--w3cos-ime-keep".to_string(), "1".to_string()),
    ]));
    Component::button_with_click(label, style, invoke(OP_SOFTKEY))
}

fn key_row(width: f32, children: Vec<Component>) -> Component {
    Component::row(
        Style {
            width: Dimension::Px(width),
            height: Dimension::Px(KEY_ROW),
            gap: KEY_GAP,
            align_items: AlignItems::Center,
            flex_shrink: 0.0,
            overflow: Overflow::Hidden,
            box_sizing: BoxSizing::BorderBox,
            ..Style::default()
        },
        children,
    )
}

fn letter_row(letters: &[&str], key_w: f32) -> Component {
    let width = key_w * letters.len() as f32 + KEY_GAP * letters.len().saturating_sub(1) as f32;
    key_row(
        width,
        letters
            .iter()
            .map(|letter| soft_key((*letter).to_string(), letter, key_w, false))
            .collect(),
    )
}

/// Basic QWERTY keyboard for the 390×760 phone posture, including the pinyin toggle.
pub(crate) fn phone_keyboard() -> Option<Component> {
    if !keyboard_open() || with_session(|session| session.ink_open) {
        return None;
    }
    let (width, height) = state::shell_viewport();
    let inner = keyboard_inner(width);
    let key10 = even_key(inner, 10);
    let home = ["a", "s", "d", "f", "g", "h", "j", "k", "l"];
    let home_w = key10;
    let bottom_letters = 7.0;
    let mode_w = 44.0_f32.min(inner * 0.16);
    let back_w = 72.0_f32.min(inner * 0.24);
    let letter_budget = inner - mode_w - back_w - KEY_GAP * 8.0;
    let letter_w = (letter_budget / bottom_letters * 10.0).floor() / 10.0;
    let bottom_w = mode_w + letter_w * bottom_letters + back_w + KEY_GAP * 8.0;
    let enter_w = 84.0_f32.min(inner * 0.28);
    let space_w = (inner - enter_w - KEY_GAP).max(0.0);
    let mut mode = mode_button();
    mode.style.width = Dimension::Px(mode_w);
    mode.style.min_width = Dimension::Px(mode_w);
    mode.style.max_width = Dimension::Px(mode_w);
    mode.style.flex_grow = 0.0;
    mode.style.flex_shrink = 0.0;
    mode.style.height = Dimension::Px(KEY_ROW);
    mode.style.min_height = Dimension::Px(KEY_ROW);
    let mut board = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(0.0),
            top: Dimension::Px((height - KEYBOARD_HEIGHT).max(0.0)),
            width: Dimension::Px(width),
            height: Dimension::Px(KEYBOARD_HEIGHT),
            padding: Edges::xy(KEY_MARGIN, 4.0),
            gap: 6.0,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            background: Color::from_hex("#10161d"),
            border_width: KEY_BORDER,
            border_color: line_strong(),
            box_sizing: BoxSizing::BorderBox,
            overflow: Overflow::Hidden,
            z_index: CHROME_Z + 6,
            flex_shrink: 0.0,
            ..Style::default()
        },
        vec![
            letter_row(&["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"], key10),
            letter_row(&home, home_w),
            key_row(
                bottom_w.min(inner),
                vec![
                    mode,
                    soft_key("z".into(), "z", letter_w, false),
                    soft_key("x".into(), "x", letter_w, false),
                    soft_key("c".into(), "c", letter_w, false),
                    soft_key("v".into(), "v", letter_w, false),
                    soft_key("b".into(), "b", letter_w, false),
                    soft_key("n".into(), "n", letter_w, false),
                    soft_key("m".into(), "m", letter_w, false),
                    soft_key(locale::t("kbd-backspace"), "Backspace", back_w, true),
                ],
            ),
            key_row(
                inner,
                vec![
                    soft_key(locale::t("kbd-space"), "Space", space_w, true),
                    soft_key(locale::t("kbd-enter"), "Enter", enter_w, true),
                ],
            ),
        ],
    );
    board.style.z_index = CHROME_Z + 6;
    Some(board)
}

fn mode_button() -> Component {
    let label = if with_session(pinyin_mode) {
        locale::t("ime-mode-pinyin")
    } else {
        locale::t("ime-mode-direct")
    };
    let mut style = Style {
        background: Color::from_hex("#24384a"),
        border_radius: 8.0,
        font_size: 13.0,
        font_weight: 700,
        color: Color::WHITE,
        padding: Edges::xy(8.0, 0.0),
        height: Dimension::Px(CHIP_H),
        min_height: Dimension::Px(CHIP_H),
        flex_shrink: 0.0,
        box_sizing: BoxSizing::BorderBox,
        ..Style::default()
    };
    keep_ime(&mut style);
    Component::button_with_click(label, style, invoke(OP_IME_MODE))
}

fn action_button(offer: &CommandOffer) -> Component {
    let mark = locale::t("ime-action-mark");
    let label = locale::t_args("ime-action", &[("text", &offer.summary), ("mark", &mark)]);
    let mut style = Style {
        background: Color::from_hex("#f4f1ea"),
        border_width: 1.0,
        border_color: Color::from_hex("#d4a017"),
        border_radius: 8.0,
        font_size: 13.0,
        font_weight: 650,
        color: Color::from_hex("#1a2836"),
        padding: Edges::xy(6.0, 0.0),
        height: Dimension::Px(CHIP_H),
        min_height: Dimension::Px(CHIP_H),
        flex_shrink: 0.0,
        flex_grow: 0.0,
        box_sizing: BoxSizing::BorderBox,
        white_space: WhiteSpace::NoWrap,
        ..Style::default()
    };
    keep_ime(&mut style);
    Component::button_with_click(label, style, invoke(OP_IME_ACTION))
}

fn candidate_button(label: String, code: u8) -> Component {
    let mut style = Style {
        background: Color::from_hex("#1c8f62"),
        border_radius: 8.0,
        font_size: 13.0,
        font_weight: 650,
        color: Color::WHITE,
        padding: Edges::xy(6.0, 0.0),
        height: Dimension::Px(CHIP_H),
        min_height: Dimension::Px(CHIP_H),
        flex_shrink: 0.0,
        flex_grow: 0.0,
        box_sizing: BoxSizing::BorderBox,
        ..Style::default()
    };
    keep_ime(&mut style);
    Component::button_with_click(label, style, invoke(code))
}

fn ink_candidate_children(candidates: &[String], page: usize) -> Vec<Component> {
    let size = page_size();
    let pages = candidates.len().div_ceil(size).max(1);
    let page = page.min(pages - 1);
    let mut children = vec![mode_button()];
    if page > 0 {
        children.push(candidate_button(
            locale::t("ime-page-prev"),
            OP_IME_PAGE_PREV,
        ));
    }
    for (index, text) in candidates.iter().skip(page * size).take(size).enumerate() {
        let n = (index + 1).to_string();
        let label = locale::t_args("ime-candidate", &[("n", &n), ("text", text)]);
        children.push(candidate_button(label, OP_IME_1 + index as u8));
    }
    if page + 1 < pages {
        children.push(candidate_button(
            locale::t("ime-page-next"),
            OP_IME_PAGE_NEXT,
        ));
    }
    children
}

fn ink_chip_ready() -> bool {
    with_session(|session| session.ink_open) || ink_engine_ready()
}

fn ink_chip_key() -> &'static str {
    if with_session(|session| session.ink_open) {
        "ime-ink-live"
    } else if ink_engine_ready() {
        "ime-ink-ready"
    } else {
        "ime-ink"
    }
}

fn ink_engine_ready() -> bool {
    HAND.with(|slot| {
        let manifest = slot.borrow().manifest();
        manifest.available && !manifest.network
    })
}

fn handwriting_dir() -> std::path::PathBuf {
    HAND_DIR.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(default_handwriting_dir)
    })
}

fn note_ink_toast(key: &'static str) {
    INK_TOAST.with(|slot| slot.set(Some(key)));
}

pub(crate) fn take_ink_toast() -> Option<&'static str> {
    INK_TOAST.with(|slot| slot.take())
}

fn close_ink(session: &mut Session) {
    session.ink_open = false;
    session.ink_strokes.clear();
    session.ink_candidates.clear();
}

struct InkGeom {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
}

fn ink_geom(session: &Session) -> InkGeom {
    let (width, height) = state::shell_viewport();
    let (x, y, w, h) = if width < 600.0 {
        (
            0.0,
            (height - KEYBOARD_HEIGHT).max(0.0),
            width,
            KEYBOARD_HEIGHT,
        )
    } else {
        let (cx, cy) = place(width, height, &session.caret);
        let w = bar_width(width);
        let h = 200.0;
        let y = (cy + BAR_H + 8.0).min((height - h - 8.0).max(8.0));
        (cx, y, w, h)
    };
    let sx = x + 8.0;
    let sy = y + 8.0;
    let sw = (w - 16.0).max(8.0);
    let sh = (h - 64.0).max(8.0);
    InkGeom {
        x,
        y,
        w,
        h,
        sx,
        sy,
        sw,
        sh,
    }
}

fn local_ink(x: f32, y: f32, geom: &InkGeom) -> (f32, f32) {
    (
        (x - geom.sx).clamp(0.0, geom.sw),
        (y - geom.sy).clamp(0.0, geom.sh),
    )
}

fn commit_ink_at(session: &mut Session, index: usize) -> bool {
    let Some(chunk) = session.ink_candidates.get(index).cloned() else {
        return false;
    };
    if !session.secure {
        crate::lexicon::note_ink_pick(&learning_scope(), &chunk);
    }
    session.base.push_str(&chunk);
    session.undo.push(chunk);
    session.ink_strokes.clear();
    session.ink_candidates.clear();
    session.page = 0;
    sync_note_value(session);
    refresh(session);
    queue(session, session.base.clone());
    note_ink_toast("ime-toast-ink-ok");
    true
}

fn recognize_open_ink(session: &mut Session) {
    let Some(ime) = session.ime.clone() else {
        session.ink_candidates.clear();
        return;
    };
    let geom = ink_geom(session);
    let result = HAND.with(|slot| {
        recognize_ink(
            &mut **slot.borrow_mut(),
            &ime,
            geom.sw.max(1.0) as u32,
            geom.sh.max(1.0) as u32,
            &session.ink_strokes,
        )
    });
    session.ink_candidates = match result {
        InkResult::Candidates(list) => list.into_iter().map(|item| item.text).collect(),
        InkResult::Off(_) => Vec::new(),
    };
}

/// Pointer samples for the open pad. Phase 0 begins, 1 extends, 2 recognizes.
pub(crate) fn ink_pointer(phase: u8, x: f32, y: f32) {
    with_session(|session| {
        if !session.ink_open || session.secure || !session.has_field {
            return;
        }
        let geom = ink_geom(session);
        match phase {
            0 => {
                let (px, py) = local_ink(x, y, &geom);
                session.ink_strokes.push(InkStroke {
                    points: vec![InkPoint { x: px, y: py }],
                });
                if session.ink_strokes.len() > 64 {
                    session.ink_strokes.remove(0);
                }
                state::mark_dirty();
            }
            1 => {
                let (px, py) = local_ink(x, y, &geom);
                let Some(stroke) = session.ink_strokes.last_mut() else {
                    return;
                };
                if let Some(last) = stroke.points.last() {
                    let dx = last.x - px;
                    let dy = last.y - py;
                    if dx * dx + dy * dy < 2.25 {
                        return;
                    }
                }
                if stroke.points.len() < 512 {
                    stroke.points.push(InkPoint { x: px, y: py });
                    state::mark_dirty();
                }
            }
            2 => {
                session.page = 0;
                recognize_open_ink(session);
                state::mark_dirty();
            }
            _ => {}
        }
    });
}

/// Drop the current strokes. The pad stays open and nothing is learned.
pub(crate) fn clear_ink() {
    with_session(|session| {
        if !session.ink_open {
            return;
        }
        session.ink_strokes.clear();
        session.ink_candidates.clear();
        session.page = 0;
        state::mark_dirty();
    });
}

pub(crate) fn ink_model_status_key() -> &'static str {
    if handwriting_model_ready(&handwriting_dir()) {
        "ime-ink-model-ready"
    } else {
        "ime-ink-model-off"
    }
}

pub(crate) fn ink_model_button_key() -> Option<&'static str> {
    if handwriting_model_ready(&handwriting_dir()) {
        None
    } else if INK_CONSENT.with(Cell::get) {
        Some("ime-ink-model-confirm")
    } else {
        Some("ime-ink-model-get")
    }
}

/// First press only arms consent. The second press downloads. Recognition does not call this.
pub(crate) fn confirm_handwriting_model() -> &'static str {
    if handwriting_model_ready(&handwriting_dir()) {
        INK_CONSENT.with(|flag| flag.set(false));
        return "ime-toast-ink-saved";
    }
    if !INK_CONSENT.with(Cell::get) {
        INK_CONSENT.with(|flag| flag.set(true));
        return "ime-toast-ink-confirm";
    }
    INK_CONSENT.with(|flag| flag.set(false));
    match download_handwriting_model(&handwriting_dir(), true) {
        Ok(_) => "ime-toast-ink-saved",
        Err(InkModelError::ChecksumMismatch { .. }) => "ime-toast-ink-checksum",
        Err(InkModelError::ConsentRequired) => "ime-toast-ink-confirm",
        Err(_) => "ime-toast-ink-download-fail",
    }
}

/// Toggle the pad. Without a local model this only explains why, and does not recognize.
pub(crate) fn ink_tap() -> Option<&'static str> {
    with_session(|session| {
        adopt_for_capture(session);
        if !session.has_field {
            return Some("ime-toast-ink-closed");
        }
        if session.secure
            || session
                .ime
                .as_ref()
                .is_some_and(|ime| ime.purpose.is_secure())
        {
            close_ink(session);
            return Some("ime-toast-ink-secure");
        }
        let Some(ime) = session.ime.clone() else {
            return Some("ime-toast-ink-closed");
        };
        if ime.id.is_empty() {
            return Some("ime-toast-ink-closed");
        }
        let manifest = HAND.with(|slot| slot.borrow().manifest());
        if manifest.network {
            return Some("ime-toast-ink-net");
        }
        if !manifest.available {
            return Some(if handwriting_model_ready(&handwriting_dir()) {
                "ime-toast-ink"
            } else {
                "ime-toast-ink-offer"
            });
        }
        if session.ink_open {
            close_ink(session);
            state::mark_dirty();
            return None;
        }
        stop_voice(session);
        abandon_screen(session);
        session.spelling.clear();
        session.preedit.clear();
        session.candidates.clear();
        session.page = 0;
        session.ink_strokes.clear();
        session.ink_candidates.clear();
        session.ink_open = true;
        refresh(session);
        state::mark_dirty();
        Some("ime-toast-ink-ready")
    })
}

/// Screenshot hook. Opens the pad once and seeds a horizontal stroke.
pub(crate) fn show_ink_pad_for_screen() {
    if ink_tap().is_none() && !with_session(|session| session.ink_open) {
        return;
    }
    with_session(|session| {
        if !session.ink_open || !session.ink_strokes.is_empty() {
            return;
        }
        let geom = ink_geom(session);
        let at = |x: f32, y: f32| InkPoint {
            x: geom.sw * x,
            y: geom.sh * y,
        };
        session.ink_strokes = vec![
            InkStroke {
                points: vec![at(22.0 / 48.0, 6.0 / 48.0), at(6.0 / 48.0, 42.0 / 48.0)],
            },
            InkStroke {
                points: vec![at(18.0 / 48.0, 20.0 / 48.0), at(42.0 / 48.0, 42.0 / 48.0)],
            },
        ];
        recognize_open_ink(session);
        state::mark_dirty();
    });
}

/// A queued permission decision for the shell receipt log. Capture does not
/// write the receipt itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScreenDecision {
    Allowed,
    Cancelled,
}

pub(crate) fn take_screen_decision() -> Option<ScreenDecision> {
    match SCREEN_DECISION.with(|slot| slot.replace(0)) {
        1 => Some(ScreenDecision::Allowed),
        2 => Some(ScreenDecision::Cancelled),
        _ => None,
    }
}

fn queue_screen_decision(code: u8) {
    SCREEN_DECISION.with(|slot| slot.set(code));
}

fn note_screen_toast(key: &'static str) {
    SCREEN_TOAST.with(|slot| slot.set(Some(key)));
}

pub(crate) fn take_screen_toast() -> Option<&'static str> {
    SCREEN_TOAST.with(|slot| slot.take())
}

fn abandon_screen(session: &mut Session) {
    if matches!(session.screen_phase, ScreenPhase::Drag | ScreenPhase::Read) {
        queue_screen_decision(2);
        w3cos_runtime::frame_cache::clear();
    }
    session.screen_phase = ScreenPhase::Idle;
    session.screen_candidates.clear();
    session.screen_dragging = false;
    SCREEN_PIXELS.with(|slot| slot.borrow_mut().take());
}

fn screen_engine_ready() -> bool {
    SCREEN.with(|slot| {
        let manifest = slot.borrow().manifest();
        manifest.available && !manifest.network
    })
}

fn screen_chip_ready() -> bool {
    with_session(|session| session.screen_phase != ScreenPhase::Idle) || screen_engine_ready()
}

fn screen_chip_key() -> &'static str {
    match with_session(|session| session.screen_phase) {
        ScreenPhase::Ask => "ime-screen-ask",
        ScreenPhase::Drag | ScreenPhase::Read => "ime-screen-live",
        ScreenPhase::Idle if screen_engine_ready() => "ime-screen-ready",
        ScreenPhase::Idle => "ime-screen",
    }
}

fn screen_dir() -> std::path::PathBuf {
    SCREEN_DIR.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(default_screen_text_dir)
    })
}

pub(crate) fn screen_model_status_key() -> &'static str {
    if screen_text_ready(&screen_dir(), &handwriting_dir()) {
        "ime-screen-model-ready"
    } else {
        "ime-screen-model-off"
    }
}

pub(crate) fn screen_model_button_key() -> Option<&'static str> {
    if screen_text_ready(&screen_dir(), &handwriting_dir()) {
        None
    } else if SCREEN_CONSENT.with(Cell::get) {
        Some("ime-screen-model-confirm")
    } else {
        Some("ime-screen-model-get")
    }
}

/// First press only arms consent. The second press downloads. Recognition does not call this.
pub(crate) fn confirm_screen_model() -> &'static str {
    if screen_text_ready(&screen_dir(), &handwriting_dir()) {
        SCREEN_CONSENT.with(|flag| flag.set(false));
        return "ime-toast-screen-saved";
    }
    if !SCREEN_CONSENT.with(Cell::get) {
        SCREEN_CONSENT.with(|flag| flag.set(true));
        return "ime-toast-screen-confirm";
    }
    SCREEN_CONSENT.with(|flag| flag.set(false));
    match download_screen_text_model(&screen_dir(), &handwriting_dir(), true) {
        Ok(()) => "ime-toast-screen-saved",
        Err(ScreenModelError::ChecksumMismatch { .. }) => "ime-toast-screen-checksum",
        Err(ScreenModelError::ConsentRequired) => "ime-toast-screen-confirm",
        Err(_) => "ime-toast-screen-download-fail",
    }
}

/// Ask for one region. Without a local model this only explains why.
pub(crate) fn screen_tap() -> Option<&'static str> {
    with_session(|session| {
        adopt_for_capture(session);
        if !session.has_field {
            return Some("ime-toast-screen-closed");
        }
        if session.secure
            || session
                .ime
                .as_ref()
                .is_some_and(|ime| ime.purpose.is_secure())
        {
            abandon_screen(session);
            return Some("ime-toast-screen-secure");
        }
        let Some(ime) = session.ime.clone() else {
            return Some("ime-toast-screen-closed");
        };
        if ime.id.is_empty() {
            return Some("ime-toast-screen-closed");
        }
        let manifest = SCREEN.with(|slot| slot.borrow().manifest());
        if manifest.network {
            return Some("ime-toast-screen-net");
        }
        if !manifest.available {
            return Some(if screen_text_ready(&screen_dir(), &handwriting_dir()) {
                "ime-toast-screen"
            } else {
                "ime-toast-screen-offer"
            });
        }
        if session.screen_phase != ScreenPhase::Idle {
            let asking = session.screen_phase == ScreenPhase::Ask;
            if asking {
                queue_screen_decision(2);
            }
            abandon_screen(session);
            state::mark_dirty();
            return Some("ime-toast-screen-cancel");
        }
        stop_voice(session);
        close_ink(session);
        session.spelling.clear();
        session.preedit.clear();
        session.candidates.clear();
        session.page = 0;
        session.screen_candidates.clear();
        session.screen_dragging = false;
        session.screen_phase = ScreenPhase::Ask;
        refresh(session);
        state::mark_dirty();
        Some("ime-toast-screen-ask")
    })
}

/// Confirm the proposed capture. The region is still not read.
pub(crate) fn screen_allow() -> Option<&'static str> {
    with_session(|session| {
        adopt_for_capture(session);
        if session.secure
            || session
                .ime
                .as_ref()
                .is_some_and(|ime| ime.purpose.is_secure())
        {
            abandon_screen(session);
            return Some("ime-toast-screen-secure");
        }
        if session.screen_phase != ScreenPhase::Ask {
            return None;
        }
        queue_screen_decision(1);
        session.screen_phase = ScreenPhase::Drag;
        session.screen_dragging = false;
        state::mark_dirty();
        Some("ime-toast-screen-allow")
    })
}

/// Refuse the proposal, or leave the selector before a region is read.
pub(crate) fn screen_cancel() -> Option<&'static str> {
    with_session(|session| {
        if session.screen_phase == ScreenPhase::Idle {
            return None;
        }
        queue_screen_decision(2);
        session.screen_phase = ScreenPhase::Idle;
        session.screen_dragging = false;
        session.screen_candidates.clear();
        SCREEN_PIXELS.with(|slot| slot.borrow_mut().take());
        w3cos_runtime::frame_cache::clear();
        state::mark_dirty();
        Some("ime-toast-screen-cancel")
    })
}

pub(crate) fn screen_drag_active() -> bool {
    with_session(|session| session.screen_phase == ScreenPhase::Drag)
}

/// Pointer samples for the open selector. Phase 0 begins, 1 moves, 2 reads once.
pub(crate) fn screen_pointer(phase: u8, x: f32, y: f32) {
    with_session(|session| {
        if session.screen_phase != ScreenPhase::Drag || session.secure || !session.has_field {
            return;
        }
        match phase {
            0 => {
                session.screen_dragging = true;
                session.screen_x0 = x;
                session.screen_y0 = y;
                session.screen_x1 = x;
                session.screen_y1 = y;
                state::mark_dirty();
            }
            1 => {
                if !session.screen_dragging {
                    return;
                }
                session.screen_x1 = x;
                session.screen_y1 = y;
                state::mark_dirty();
            }
            2 => {
                if !session.screen_dragging {
                    return;
                }
                session.screen_dragging = false;
                release_screen_region(session);
            }
            _ => {}
        }
    });
}

/// One requested frame, after the selector is hidden. Older cache entries are ignored.
pub(crate) fn poll_screen() -> bool {
    with_session(|session| {
        if session.screen_phase != ScreenPhase::Read {
            return false;
        }
        let Some((width, height, rgba, generation)) = w3cos_runtime::frame_cache::snapshot() else {
            return false;
        };
        if generation <= session.screen_wait_gen {
            return false;
        }
        finish_screen(
            session,
            ScreenImage {
                width,
                height,
                rgba,
            },
        );
        true
    })
}

fn release_screen_region(session: &mut Session) {
    let width = (session.screen_x0 - session.screen_x1).abs();
    let height = (session.screen_y0 - session.screen_y1).abs();
    if width < 8.0 || height < 8.0 {
        note_screen_toast("ime-toast-screen-small");
        state::mark_dirty();
        return;
    }
    if let Some(image) = SCREEN_PIXELS.with(|slot| slot.borrow_mut().take()) {
        finish_screen(session, image);
        return;
    }
    session.screen_wait_gen = w3cos_runtime::frame_cache::snapshot()
        .map(|frame| frame.3)
        .unwrap_or(0);
    session.screen_phase = ScreenPhase::Read;
    w3cos_runtime::frame_cache::request_screenshot();
    state::note_shell_poll_soon(true);
    state::mark_dirty();
}

fn map_screen_region(session: &Session, image: &ScreenImage) -> ScreenRegion {
    let (viewport_w, viewport_h) = state::shell_viewport();
    let left = session.screen_x0.min(session.screen_x1).max(0.0);
    let top = session.screen_y0.min(session.screen_y1).max(0.0);
    let right = session.screen_x0.max(session.screen_x1);
    let bottom = session.screen_y0.max(session.screen_y1);
    let scale_x = image.width as f32 / viewport_w.max(1.0);
    let scale_y = image.height as f32 / viewport_h.max(1.0);
    let x = (left * scale_x).floor().clamp(0.0, image.width as f32) as u32;
    let y = (top * scale_y).floor().clamp(0.0, image.height as f32) as u32;
    let x1 = (right * scale_x).ceil().clamp(0.0, image.width as f32) as u32;
    let y1 = (bottom * scale_y).ceil().clamp(0.0, image.height as f32) as u32;
    ScreenRegion {
        x,
        y,
        width: x1.saturating_sub(x),
        height: y1.saturating_sub(y),
    }
}

fn finish_screen(session: &mut Session, image: ScreenImage) {
    let region = map_screen_region(session, &image);
    session.screen_phase = ScreenPhase::Idle;
    session.screen_dragging = false;
    w3cos_runtime::frame_cache::clear();
    let Some(ime) = session.ime.clone().filter(|ime| !ime.id.is_empty()) else {
        note_screen_toast("ime-toast-screen-closed");
        state::mark_dirty();
        return;
    };
    let result =
        SCREEN.with(|slot| recognize_screen(&mut **slot.borrow_mut(), &ime, &image, region, true));
    drop(image);
    session.page = 0;
    match result {
        ScreenResult::Snippets(list) if !list.is_empty() => {
            session.screen_candidates = list.into_iter().map(|item| item.text).collect();
            note_screen_toast("ime-toast-screen-ready");
        }
        ScreenResult::Off(ScreenOff::SecureField) => {
            note_screen_toast("ime-toast-screen-secure");
        }
        ScreenResult::Off(ScreenOff::NoSession) => {
            note_screen_toast("ime-toast-screen-closed");
        }
        ScreenResult::Off(ScreenOff::NetworkRefused) => {
            note_screen_toast("ime-toast-screen-net");
        }
        ScreenResult::Off(ScreenOff::NoLocalModel) => note_screen_toast("ime-toast-screen"),
        ScreenResult::Off(ScreenOff::Empty | ScreenOff::Unconfirmed) => {
            note_screen_toast("ime-toast-screen-small");
        }
        ScreenResult::Snippets(_) => note_screen_toast("ime-toast-screen-empty"),
    }
    state::mark_dirty();
}

fn commit_screen_at(session: &mut Session, index: usize) -> bool {
    let Some(chunk) = session.screen_candidates.get(index).cloned() else {
        return false;
    };
    if !session.secure {
        crate::lexicon::note_screen_pick(&learning_scope(), &chunk);
    }
    session.base.push_str(&chunk);
    session.undo.push(chunk);
    session.screen_candidates.clear();
    session.page = 0;
    sync_note_value(session);
    refresh(session);
    queue(session, session.base.clone());
    note_screen_toast("ime-toast-screen-ok");
    true
}

fn screen_surface_geom() -> (f32, f32, f32, f32) {
    let (width, height) = state::shell_viewport();
    (0.0, 48.0, width, (height - 48.0).max(8.0))
}

pub(crate) fn screen_layer() -> Option<Component> {
    let phase = with_session(|session| {
        if session.secure || !session.has_field {
            return ScreenPhase::Idle;
        }
        session.screen_phase
    });
    match phase {
        ScreenPhase::Ask => Some(screen_ask_card()),
        ScreenPhase::Drag => Some(screen_drag_layer()),
        ScreenPhase::Idle | ScreenPhase::Read => None,
    }
}

fn screen_ask_card() -> Component {
    let (width, height) = state::shell_viewport();
    let card_w = (width - 48.0).clamp(280.0, 440.0);
    let mut card = Component::column(
        Style {
            width: Dimension::Px(card_w),
            padding: Edges::all(16.0),
            gap: 12.0,
            background: Color::from_hex("#162433"),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 16.0,
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t("ime-screen-title"),
                Style {
                    font_size: 18.0,
                    font_weight: 700,
                    color: Color::from_hex("#f4f7fb"),
                    ..Style::default()
                },
            ),
            Component::text(
                locale::t("ime-screen-impact"),
                Style {
                    font_size: 14.0,
                    color: Color::from_hex("#c5d0dc"),
                    ..Style::default()
                },
            ),
            Component::row(
                Style {
                    height: Dimension::Px(40.0),
                    gap: 8.0,
                    ..Style::default()
                },
                vec![
                    ink_pad_button(
                        &locale::t("confirm"),
                        OP_IME_SCREEN_YES,
                        (card_w - 40.0) * 0.5,
                    ),
                    ink_pad_button(
                        &locale::t("cancel"),
                        OP_IME_SCREEN_NO,
                        (card_w - 40.0) * 0.5,
                    ),
                ],
            ),
        ],
    );
    keep_ime(&mut card.style);
    mark_prop(&mut card.style, "--w3cos-screen", "ask");
    let mut wrap = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(((width - card_w) * 0.5).max(16.0)),
            top: Dimension::Px((height * 0.28).clamp(24.0, 220.0)),
            z_index: CHROME_Z + 8,
            ..Style::default()
        },
        vec![card],
    );
    keep_ime(&mut wrap.style);
    wrap
}

fn screen_drag_layer() -> Component {
    let (width, height) = state::shell_viewport();
    let (sx, sy, sw, sh) = screen_surface_geom();
    let selection = with_session(|session| {
        if !session.screen_dragging {
            return None;
        }
        let left = (session.screen_x0.min(session.screen_x1) - sx).clamp(0.0, sw);
        let top = (session.screen_y0.min(session.screen_y1) - sy).clamp(0.0, sh);
        let rect_w = (session.screen_x0 - session.screen_x1).abs().min(sw - left);
        let rect_h = (session.screen_y0 - session.screen_y1).abs().min(sh - top);
        Some((left, top, rect_w.max(1.0), rect_h.max(1.0)))
    });
    let mut surface_style = Style {
        width: Dimension::Px(sw),
        height: Dimension::Px(sh),
        min_height: Dimension::Px(sh),
        flex_shrink: 0.0,
        background: Color::from_hex("#07101899"),
        ..Style::default()
    };
    keep_ime(&mut surface_style);
    mark_prop(&mut surface_style, "--w3cos-ink", "1");
    mark_prop(&mut surface_style, "--w3cos-screen", "region");
    let mut surface_children = vec![Component::text(
        locale::t("ime-screen-hint"),
        Style {
            font_size: 15.0,
            font_weight: 650,
            color: Color::from_hex("#f4f7fb"),
            margin: Edges::xy(16.0, 12.0),
            ..Style::default()
        },
    )];
    if let Some((left, top, rect_w, rect_h)) = selection {
        surface_children.push(Component::column(
            Style {
                position: Position::Absolute,
                left: Dimension::Px(left),
                top: Dimension::Px(top),
                width: Dimension::Px(rect_w),
                height: Dimension::Px(rect_h),
                border_width: 2.0,
                border_color: Color::from_hex("#f0d7b4"),
                background: Color::from_hex("#f0d7b433"),
                ..Style::default()
            },
            Vec::new(),
        ));
    }
    let mut surface = Component::column(surface_style, surface_children);
    surface.on_click = invoke(OP_SCRIM);
    let mut layer = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(0.0),
            top: Dimension::Px(0.0),
            width: Dimension::Px(width),
            height: Dimension::Px(height),
            background: Color::from_hex("#071018cc"),
            z_index: CHROME_Z + 8,
            ..Style::default()
        },
        vec![
            Component::row(
                Style {
                    height: Dimension::Px(sy),
                    padding: Edges::xy(12.0, 6.0),
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        locale::t("ime-screen-live"),
                        Style {
                            font_size: 14.0,
                            font_weight: 700,
                            color: Color::from_hex("#f4f7fb"),
                            flex_grow: 1.0,
                            ..Style::default()
                        },
                    ),
                    ink_pad_button(&locale::t("cancel"), OP_IME_SCREEN_NO, 96.0),
                ],
            ),
            surface,
        ],
    );
    keep_ime(&mut layer.style);
    mark_prop(&mut layer.style, "--w3cos-screen", "live");
    layer.on_click = invoke(OP_SCRIM);
    layer
}

/// Screenshot hook. Arms the download button without opening a socket.
pub(crate) fn arm_screen_model_confirm_for_screen() {
    if !screen_text_ready(&screen_dir(), &handwriting_dir()) {
        SCREEN_CONSENT.with(|flag| flag.set(true));
    }
}

/// Screenshot hook. Opens the permission card when a local engine is ready.
pub(crate) fn show_screen_ask_for_screen() {
    if !screen_engine_ready() {
        return;
    }
    with_session(|session| {
        adopt_for_capture(session);
        if session.secure || !session.has_field {
            return;
        }
        close_ink(session);
        session.screen_candidates.clear();
        session.screen_phase = ScreenPhase::Ask;
        state::mark_dirty();
    });
}

/// Screenshot hook. Shows the region indicator without reading pixels.
pub(crate) fn show_screen_region_for_screen() {
    if !screen_engine_ready() {
        return;
    }
    with_session(|session| {
        adopt_for_capture(session);
        if session.secure || !session.has_field {
            return;
        }
        close_ink(session);
        let (width, height) = state::shell_viewport();
        session.screen_phase = ScreenPhase::Drag;
        session.screen_dragging = true;
        session.screen_x0 = width * 0.18;
        session.screen_y0 = height * 0.28;
        session.screen_x1 = width * 0.72;
        session.screen_y1 = height * 0.52;
        state::mark_dirty();
    });
}

fn ink_svg(strokes: &[InkStroke], width: f32, height: f32) -> String {
    let mut paths = String::new();
    for stroke in strokes {
        let Some(first) = stroke.points.first() else {
            continue;
        };
        let mut data = format!("M {:.1} {:.1}", first.x, first.y);
        for point in stroke.points.iter().skip(1) {
            data.push_str(&format!(" L {:.1} {:.1}", point.x, point.y));
        }
        paths.push_str(&format!(
            "<path d=\"{data}\" fill=\"none\" stroke=\"#1a1a1a\" stroke-width=\"3.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
        ));
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">{paths}</svg>"
    )
}

fn mark_prop(style: &mut Style, key: &str, value: &str) {
    let mut props = style.custom_properties.take().unwrap_or_default();
    props.insert(key.to_string(), value.to_string());
    style.custom_properties = Some(props);
}

/// Drawing surface. Hidden for secure fields. Phone pad replaces the keyboard rect.
pub(crate) fn ink_pad_layer() -> Option<Component> {
    let drawn = with_session(|session| {
        if !session.ink_open || !session.has_field || session.secure {
            return None;
        }
        Some((ink_geom(session), session.ink_strokes.clone()))
    })?;
    if field_blocks_capture() {
        return None;
    }
    let (geom, strokes) = drawn;
    let phone = state::shell_viewport().0 < 600.0;
    let mut surface_style = Style {
        width: Dimension::Px(geom.sw),
        height: Dimension::Px(geom.sh),
        min_height: Dimension::Px(geom.sh),
        flex_shrink: 0.0,
        background: Color::from_hex("#f7f4ee"),
        border_radius: 10.0,
        border_width: 1.0,
        border_color: Color::from_hex("#d7c7a2"),
        overflow: Overflow::Hidden,
        align_items: AlignItems::Center,
        justify_content: JustifyContent::Center,
        ..Style::default()
    };
    keep_ime(&mut surface_style);
    mark_prop(&mut surface_style, "--w3cos-ink", "1");
    let surface_child = if strokes.is_empty() {
        Component::text(
            locale::t("ime-ink-hint"),
            Style {
                font_size: 15.0,
                font_weight: 650,
                color: Color::from_hex("#8a7b62"),
                ..Style::default()
            },
        )
    } else {
        Component::svg_document(
            ink_svg(&strokes, geom.sw, geom.sh),
            geom.sw.max(1.0) as u32,
            geom.sh.max(1.0) as u32,
            Style {
                width: Dimension::Px(geom.sw),
                height: Dimension::Px(geom.sh),
                ..Style::default()
            },
        )
    };
    let mut surface = Component::column(surface_style, vec![surface_child]);
    surface.on_click = invoke(OP_SCRIM);
    let button_w = ((geom.w - 16.0 - 8.0) * 0.5).max(48.0);
    let mut pad = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(geom.x),
            top: Dimension::Px(geom.y),
            width: Dimension::Px(geom.w),
            height: Dimension::Px(geom.h),
            padding: Edges::all(8.0),
            gap: 8.0,
            background: Color::from_hex("#10161d"),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: if phone { 0.0 } else { 12.0 },
            box_sizing: BoxSizing::BorderBox,
            overflow: Overflow::Hidden,
            z_index: if phone { CHROME_Z + 6 } else { CHROME_Z + 4 },
            ..Style::default()
        },
        vec![
            surface,
            Component::row(
                Style {
                    height: Dimension::Px(40.0),
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
                vec![
                    ink_pad_button(&locale::t("ime-ink-clear"), OP_IME_INK_CLEAR, button_w),
                    ink_pad_button(&locale::t("ime-ink-close"), OP_IME_INK, button_w),
                ],
            ),
        ],
    );
    keep_ime(&mut pad.style);
    pad.on_click = invoke(OP_SCRIM);
    pad.style.z_index = if phone { CHROME_Z + 6 } else { CHROME_Z + 4 };
    Some(pad)
}

fn ink_pad_button(label: &str, code: u8, width: f32) -> Component {
    let mut style = Style {
        width: Dimension::Px(width),
        min_width: Dimension::Px(width),
        height: Dimension::Px(36.0),
        min_height: Dimension::Px(36.0),
        flex_grow: 1.0,
        background: Color::from_hex("#24384a"),
        border_radius: 8.0,
        font_size: 14.0,
        font_weight: 650,
        color: Color::from_hex("#f4f7fb"),
        ..Style::default()
    };
    keep_ime(&mut style);
    Component::button_with_click(label.to_string(), style, invoke(code))
}

pub(crate) fn demo_compose(spelling: &str) {
    with_session(|session| {
        session.secure = false;
        session.has_field = true;
        session.role = "intent".into();
        session.override_pinyin = Some(true);
        session.spelling = spelling.to_string();
        session.base.clear();
        session.page = 0;
        refresh(session);
    });
}

#[cfg(test)]
pub(crate) fn candidate_texts() -> Vec<String> {
    with_session(|session| session.candidates.clone())
}

#[cfg(test)]
pub(crate) fn set_speech_dir_for_tests(dir: Option<std::path::PathBuf>) {
    SPEECH_DIR.with(|slot| *slot.borrow_mut() = dir);
    MODEL_CONSENT.with(|flag| flag.set(false));
}

#[cfg(test)]
pub(crate) fn set_handwriting_dir_for_tests(dir: Option<std::path::PathBuf>) {
    HAND_DIR.with(|slot| *slot.borrow_mut() = dir);
    INK_CONSENT.with(|flag| flag.set(false));
}

#[cfg(test)]
pub(crate) fn install_handwriting_for_tests(engine: Box<dyn LocalHandwriting>) {
    HAND.with(|slot| *slot.borrow_mut() = engine);
}

#[cfg(test)]
pub(crate) fn install_screen_for_tests(engine: Box<dyn LocalScreenText>) {
    SCREEN.with(|slot| *slot.borrow_mut() = engine);
}

#[cfg(test)]
pub(crate) fn install_screen_pixels_for_tests(image: ScreenImage) {
    SCREEN_PIXELS.with(|slot| *slot.borrow_mut() = Some(image));
}

#[cfg(test)]
pub(crate) fn set_screen_dir_for_tests(dir: Option<std::path::PathBuf>) {
    SCREEN_DIR.with(|slot| *slot.borrow_mut() = dir);
    SCREEN_CONSENT.with(|flag| flag.set(false));
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    SESSION.with(|slot| *slot.borrow_mut() = Session::default());
    SPEECH.with(|slot| {
        release_speech(&mut **slot.borrow_mut(), true);
        *slot.borrow_mut() = Box::new(NoLocalSpeech::default());
    });
    SPEECH_DIR.with(|slot| *slot.borrow_mut() = None);
    MODEL_CONSENT.with(|flag| flag.set(false));
    HAND.with(|slot| *slot.borrow_mut() = Box::new(NoLocalHandwriting::default()));
    HAND_DIR.with(|slot| *slot.borrow_mut() = None);
    INK_CONSENT.with(|flag| flag.set(false));
    INK_TOAST.with(|slot| slot.set(None));
    SCREEN.with(|slot| *slot.borrow_mut() = Box::new(NoLocalScreenText::default()));
    SCREEN_DIR.with(|slot| *slot.borrow_mut() = None);
    SCREEN_CONSENT.with(|flag| flag.set(false));
    SCREEN_TOAST.with(|slot| slot.set(None));
    SCREEN_DECISION.with(|slot| slot.set(0));
    SCREEN_PIXELS.with(|slot| slot.borrow_mut().take());
    state::note_shell_poll_soon(false);
    let _ = state::take_ime_replacements();
    state::clear_ime_field();
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_std::component::{ComponentKind, EventAction};
    use w3cos_std::style::{Dimension, Spacing};

    fn note(role: &str, value: &str, secure: bool, index: usize, y: f32) {
        state::note_ime_field(ImeFieldNote {
            index,
            value: value.to_string(),
            role: role.to_string(),
            secure,
            x: 120.0,
            y,
            width: 280.0,
            height: 36.0,
        });
    }

    fn type_spelling(spelling: &str) {
        for ch in spelling.chars() {
            assert!(on_key(&ch.to_string()), "{ch}");
        }
    }

    #[test]
    fn nihao_commits_and_undoes() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("intent", "", false, 4, 200.0);
        type_spelling("nihao");
        assert_eq!(composed_text(), "nihao");
        let layer = candidate_layer().expect("candidate bar");
        let dumped = format!("{layer:?}");
        assert!(dumped.contains("你好"), "{dumped}");
        assert!(on_key("1"));
        assert_eq!(composed_text(), "你好");
        assert!(candidate_layer().is_none());
        // The runtime notes the field text again before the undo key.
        note("intent", "你好", false, 4, 200.0);
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "");
        let updates = take_field_updates();
        assert!(updates.iter().any(|update| update.role == "intent"));
    }

    #[test]
    fn space_commits_the_first_candidate_and_enter_does_not_submit() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("terminal", "echo ", false, 8, 180.0);
        type_spelling("women");
        assert!(on_key("Enter"));
        assert_eq!(composed_text(), "echo 我们");
        assert!(!on_key("Enter"));
    }

    #[test]
    fn secure_fields_and_direct_mode_pass_letters_through() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("secure", "", true, 2, 300.0);
        assert!(!on_key("n"));
        assert!(candidate_layer().is_none());
        reset_for_tests();
        locale::set_locale("en");
        note("intent", "", false, 4, 200.0);
        assert!(!on_key("n"));
        assert!(on_key("Mod+Space"));
        note("intent", "", false, 4, 200.0);
        assert!(on_key("n"));
        assert_eq!(composed_text(), "n");
    }

    #[test]
    fn escape_drops_the_preedit_and_backspace_edits_it() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("text", "查", false, 6, 100.0);
        type_spelling("ni");
        assert_eq!(composed_text(), "查ni");
        assert!(on_key("Backspace"));
        assert_eq!(composed_text(), "查n");
        assert!(on_key("Escape"));
        assert_eq!(composed_text(), "查");
        assert!(!on_key("Escape"));
    }

    #[test]
    fn phone_bar_sits_above_the_keyboard_and_desktop_bar_follows_the_caret() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("intent", "", false, 4, 200.0);
        type_spelling("ni");
        let (x, y) = candidate_origin(390.0, 760.0);
        assert!(x <= 8.0);
        assert!(y + BAR_H <= 760.0 - PHONE_KEYBOARD_RESERVE + 0.5);
        let (_, desktop_y) = candidate_origin(1200.0, 800.0);
        assert!((desktop_y - (200.0 + 36.0 + 6.0)).abs() < 0.5);
        note("intent", "ni", false, 4, 740.0);
        assert!(on_key("h"));
        let (_, above) = candidate_origin(1200.0, 800.0);
        assert!(above < 740.0);
        let intent_top = (800.0_f32 - 84.0 - 78.0).max(8.0);
        assert!(above + BAR_H <= intent_top - 8.0 + 0.5);
    }

    #[test]
    fn phone_keyboard_covers_pinyin_direct_and_secure_entry() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "", false, 4, 640.0);
        assert!(matches!(on_soft_key("n"), SoftOutcome::Edit));
        assert!(matches!(on_soft_key("i"), SoftOutcome::Edit));
        assert!(matches!(on_soft_key("h"), SoftOutcome::Edit));
        assert!(matches!(on_soft_key("a"), SoftOutcome::Edit));
        assert!(matches!(on_soft_key("o"), SoftOutcome::Edit));
        let board = phone_keyboard().expect("phone keyboard");
        let dumped = format!("{board:?}");
        assert!(dumped.contains("--w3cos-softkey"), "{dumped}");
        assert!(dumped.contains("Backspace"));
        assert!(dumped.contains(&locale::t("kbd-space")));
        assert!(KEYBOARD_HEIGHT <= PHONE_KEYBOARD_RESERVE);
        let strip = phone_candidate_strip().expect("candidate strip");
        let dumped = format!("{strip:?}");
        assert!(dumped.contains("你好"), "{dumped}");
        assert!(!dumped.contains("nihao"), "{dumped}");
        assert!(!dumped.contains("ni hao"), "{dumped}");
        assert!(matches!(on_soft_key("Enter"), SoftOutcome::Edit));
        assert_eq!(composed_text(), "你好");
        assert!(matches!(
            on_soft_key("Enter"),
            SoftOutcome::Submit { role, .. } if role == "intent"
        ));

        reset_for_tests();
        locale::set_locale("en");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "hi", false, 4, 640.0);
        assert!(matches!(on_soft_key("x"), SoftOutcome::Edit));
        assert_eq!(composed_text(), "hix");
        assert!(phone_candidate_strip().is_none());
        state::note_shell_viewport(1200.0, 800.0);
        assert!(phone_keyboard().is_none());

        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("secure", "", true, 2, 400.0);
        assert!(matches!(on_soft_key("a"), SoftOutcome::Edit));
        assert_eq!(composed_text(), "a");
        assert!(phone_candidate_strip().is_none());
        assert!(phone_keyboard().is_some());
        state::clear_ime_field();
        sync_field_presence();
        assert!(phone_keyboard().is_none());
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn field_preedit_is_segmented_and_the_bar_is_one_row_above_the_intent() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        note("intent", "", false, 4, 700.0);
        type_spelling("nihao");
        assert_eq!(composed_text(), "nihao");
        assert_eq!(preedit_suffix("intent").as_deref(), Some("ni hao"));
        let updates = take_field_updates();
        assert_eq!(
            updates.last().map(|update| update.text.as_str()),
            Some("ni hao")
        );
        let layer = candidate_layer().expect("candidate bar");
        let dumped = format!("{layer:?}");
        assert!(dumped.contains("ni hao"), "{dumped}");
        assert!(dumped.contains("你好"), "{dumped}");
        assert!(!dumped.contains("你号"), "{dumped}");
        assert!(!dumped.contains("你豪"), "{dumped}");
        assert_eq!(layer.style.flex_wrap, FlexWrap::NoWrap);
        let top = px(layer.style.top);
        let height = px(layer.style.height);
        let left = px(layer.style.left);
        let width = px(layer.style.width);
        let intent_top = (800.0_f32 - 84.0 - 78.0).max(8.0);
        assert!(top + height <= intent_top - 8.0 + 0.5);
        assert!(left >= -0.5 && left + width <= 1200.0 + 0.5);
        let labels = candidate_labels(&layer);
        assert!(labels.len() <= 5, "{labels:?}");
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn long_candidate_lists_page_instead_of_wrapping() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        note("intent", "", false, 4, 200.0);
        type_spelling("yi");
        let layer = candidate_layer().expect("candidate bar");
        let first = candidate_labels(&layer);
        assert!(first.len() <= 5, "{first:?}");
        assert!(format!("{layer:?}").contains(&locale::t("ime-page-next")));
        assert!(page_by(1));
        let next = candidate_labels(&candidate_layer().expect("next page"));
        assert!(next.len() <= 5, "{next:?}");
        assert_ne!(first, next);
        assert!(on_key("1"));
        assert!(!composed_text().is_empty());
        assert_ne!(composed_text(), "yi");
    }

    #[test]
    fn purpose_follows_the_field_and_blur_clears_the_session() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("intent", "", false, 4, 200.0);
        assert!(!on_key("Escape"));
        assert_eq!(focused_purpose(), Some(Purpose::Command));
        note("terminal", "", false, 8, 180.0);
        assert!(!on_key("Escape"));
        assert_eq!(focused_purpose(), Some(Purpose::Code));
        note("editor", "", false, 9, 160.0);
        assert!(!on_key("Escape"));
        assert_eq!(focused_purpose(), Some(Purpose::Code));
        note("launcher", "", false, 3, 120.0);
        assert!(!on_key("Escape"));
        assert_eq!(focused_purpose(), Some(Purpose::Search));
        note("text", "", false, 6, 100.0);
        assert!(!on_key("Escape"));
        assert_eq!(focused_purpose(), Some(Purpose::Text));
        note("intent", "", false, 4, 200.0);
        type_spelling("ni");
        assert!(candidate_layer().is_some());
        state::clear_ime_field();
        sync_field_presence();
        assert!(focused_purpose().is_none());
        assert_eq!(composed_text(), "");
        assert!(candidate_layer().is_none());
    }

    #[test]
    fn secure_fields_skip_candidates_and_grants() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        note("secure", "", true, 2, 300.0);
        assert!(!on_key("n"));
        assert!(!on_key("i"));
        assert!(candidate_layer().is_none());
        assert_eq!(focused_purpose(), Some(Purpose::Secure));
        assert!(focused_grants_empty());
        assert!(command_utterance().is_none());
        note("password", "", true, 2, 300.0);
        assert!(!on_key("n"));
        assert_eq!(focused_purpose(), Some(Purpose::Password));
        assert!(focused_grants_empty());
    }

    #[test]
    fn command_field_offers_a_confirm_chip_and_text_still_commits() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        note("intent", "", false, 4, 700.0);
        type_spelling("wenjian");
        assert_eq!(focused_purpose(), Some(Purpose::Command));
        assert_eq!(preedit_suffix("intent").as_deref(), Some("wen jian"));
        assert_eq!(command_utterance().as_deref(), Some("open files"));
        let layer = candidate_layer().expect("command bar");
        let dumped = format!("{layer:?}");
        assert!(dumped.contains("文件"), "{dumped}");
        assert!(dumped.contains(&locale::t("ime-action-mark")), "{dumped}");
        assert_eq!(layer.style.flex_wrap, FlexWrap::NoWrap);
        assert!(on_key("1"));
        assert_eq!(composed_text(), "文件");
        assert_eq!(command_utterance().as_deref(), Some("open files"));
        note("intent", "文件", false, 4, 700.0);
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "");
        assert!(command_utterance().is_none());

        note("terminal", "", false, 8, 180.0);
        type_spelling("wenjian");
        assert_eq!(focused_purpose(), Some(Purpose::Code));
        let dumped = format!("{:?}", candidate_layer().expect("code bar"));
        assert!(dumped.contains("文件"), "{dumped}");
        assert!(!dumped.contains(&locale::t("ime-action-mark")), "{dumped}");
        assert!(command_utterance().is_none());
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn phone_keys_fit_inside_a_390_viewport() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "", false, 4, 640.0);
        let board = phone_keyboard().expect("keyboard");
        let left = px(board.style.left);
        let width = px(board.style.width);
        let top = px(board.style.top);
        let height = px(board.style.height);
        assert!(left >= -0.5 && left + width <= 390.0 + 0.5);
        assert!(top >= -0.5 && top + height <= 760.0 + 0.5);
        let pad = spacing_px(board.style.padding.left) + spacing_px(board.style.padding.right);
        let inner = width - pad - board.style.border_width * 2.0;
        assert!(pad >= 16.0, "margins should remain visible");
        assert_eq!(board.children.len(), 4);
        assert_eq!(board.children[0].children.len(), 10, "top row keeps p");
        for row in &board.children {
            let row_w = px(row.style.width);
            assert!(row_w <= inner + 0.5, "row {row_w} exceeds inner {inner}");
            let mut sum = 0.0;
            let mut count = 0usize;
            for key in &row.children {
                sum += px(key.style.width);
                count += 1;
                assert_eq!(key.style.flex_grow, 0.0);
                assert_eq!(key.style.flex_shrink, 0.0);
            }
            if count > 1 {
                sum += row.style.gap * (count - 1) as f32;
            }
            assert!(sum <= row_w + 0.5, "keys {sum} row {row_w}");
        }
        let backspace = &board.children[2].children[8];
        assert!(px(backspace.style.width) >= 64.0);
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn smart_bar_keeps_models_off_and_clipboard_text_undoes() {
        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "", false, 4, 640.0);
        let bar = phone_smart_bar().expect("phone smart bar");
        let dumped = format!("{bar:?}");
        assert!(dumped.contains(&locale::t("ime-voice")), "{dumped}");
        assert_eq!(dumped.matches("--w3cos-hold").count(), 1, "{dumped}");
        assert!(dumped.contains(&OP_IME_VOICE.to_string()), "{dumped}");
        assert!(matches!(
            voice_press(),
            SpeechStart::Off(SpeechOff::NoLocalModel)
        ));
        assert!(!voice_recording());
        assert!(!speech_capturing());
        assert_eq!(voice_release(), VoiceRelease::Idle);
        assert_eq!(composed_text(), "");
        assert!(dumped.contains(&locale::t("ime-ink")), "{dumped}");
        assert!(dumped.contains(&locale::t("ime-clip")), "{dumped}");
        assert!(dumped.contains(&locale::t("ime-screen")), "{dumped}");
        assert_eq!(bar.style.flex_wrap, FlexWrap::NoWrap);
        assert!(modality_layer().is_none());
        assert!(matches!(local_clipboard_gate(), ClipboardGate::Read));
        assert!(!crate::handoff::clipboard_sync_enabled());

        assert!(matches!(
            accept_local_clipboard(Some("  周五的评审  ")),
            ClipboardTap::Wrote
        ));
        assert_eq!(composed_text(), "周五的评审");
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "");
        assert!(matches!(
            accept_local_clipboard(Some("   ")),
            ClipboardTap::Empty
        ));
        assert_eq!(composed_text(), "");
        let huge = "字".repeat(w3cos_ime::CLIPBOARD_LIMIT + 1);
        assert!(matches!(
            accept_local_clipboard(Some(&huge)),
            ClipboardTap::TooLong
        ));
        assert_eq!(composed_text(), "");

        type_spelling("ni");
        assert!(matches!(
            accept_local_clipboard(Some("评审")),
            ClipboardTap::Wrote
        ));
        assert_eq!(composed_text(), "评审");
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "");

        assert!(matches!(
            accept_local_clipboard(Some("打开文件")),
            ClipboardTap::Wrote
        ));
        assert_eq!(composed_text(), "打开文件");
        assert_eq!(command_utterance().as_deref(), Some("open files"));
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "");
        assert!(command_utterance().is_none());
        assert!(!crate::handoff::clipboard_sync_enabled());

        note("secure", "保持", true, 2, 400.0);
        assert!(phone_smart_bar().is_none());
        assert!(phone_candidate_strip().is_none());
        assert!(matches!(local_clipboard_gate(), ClipboardGate::Secure));
        assert!(matches!(
            accept_local_clipboard(Some("泄露")),
            ClipboardTap::Secure
        ));
        assert_eq!(composed_text(), "保持");

        state::clear_ime_field();
        reset_for_tests();
        assert!(matches!(
            accept_local_clipboard(Some("无人")),
            ClipboardTap::Closed
        ));

        state::note_shell_viewport(1200.0, 800.0);
        note("intent", "", false, 4, 200.0);
        let layer = modality_layer().expect("desktop row");
        let layer_dump = format!("{layer:?}");
        assert!(layer_dump.contains(&locale::t("ime-clip")));
        assert_eq!(layer_dump.matches("--w3cos-hold").count(), 1);
        assert!(candidate_layer().is_none());
        type_spelling("ni");
        assert!(modality_layer().is_none());
        assert!(candidate_layer().is_some());
        assert!(on_key("Escape"));
        locale::set_locale("en");
        let dumped = format!("{:?}", modality_layer().expect("english row"));
        assert!(dumped.contains("Voice off"));
        assert!(dumped.contains("Ink off"));
        assert!(dumped.contains("Clipboard"));
        assert!(dumped.contains("Screen off"));

        state::note_shell_viewport(1200.0, 800.0);
        state::clear_ime_field();
        reset_for_tests();
        locale::set_locale("zh-CN");
    }

    #[test]
    fn voice_hold_streams_on_device_and_refuses_upload() {
        use std::collections::VecDeque;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        use w3cos_ime::{LocalSpeech, SpeechChunk, SpeechManifest, SpeechStart as Start};

        struct Probe {
            network: bool,
            available: bool,
            capturing: bool,
            begins: Arc<AtomicU32>,
            ends: Arc<AtomicU32>,
            chunks: VecDeque<SpeechChunk>,
        }

        impl LocalSpeech for Probe {
            fn manifest(&self) -> SpeechManifest {
                SpeechManifest {
                    id: "test.local-speech".into(),
                    locales: vec!["zh-Hans".into()],
                    network: self.network,
                    available: self.available,
                }
            }

            fn begin(&mut self, _locales: &[String]) -> Start {
                self.begins.fetch_add(1, Ordering::SeqCst);
                self.capturing = !self.network;
                if self.network {
                    Start::Off(SpeechOff::NetworkRefused)
                } else {
                    Start::Listening
                }
            }

            fn poll(&mut self) -> Option<SpeechChunk> {
                if !self.capturing {
                    return None;
                }
                self.chunks.pop_front()
            }

            fn end(&mut self) {
                self.ends.fetch_add(1, Ordering::SeqCst);
                self.capturing = false;
            }

            fn capturing(&self) -> bool {
                self.capturing
            }
        }

        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "已有", false, 4, 640.0);
        let begins = Arc::new(AtomicU32::new(0));
        let ends = Arc::new(AtomicU32::new(0));
        install_speech_for_tests(Box::new(Probe {
            network: true,
            available: true,
            capturing: false,
            begins: begins.clone(),
            ends: ends.clone(),
            chunks: VecDeque::new(),
        }));
        assert!(!voice_engine_ready());
        assert!(matches!(
            voice_press(),
            SpeechStart::Off(SpeechOff::NetworkRefused)
        ));
        assert_eq!(begins.load(Ordering::SeqCst), 0);
        assert_eq!(ends.load(Ordering::SeqCst), 0);
        assert!(!speech_capturing());
        assert!(!voice_recording());
        assert_eq!(composed_text(), "已有");
        assert!(!poll_voice());

        let begins = Arc::new(AtomicU32::new(0));
        let ends = Arc::new(AtomicU32::new(0));
        let mut chunks = VecDeque::new();
        chunks.push_back(SpeechChunk {
            text: "  改到  ".into(),
            interim: true,
        });
        chunks.push_back(SpeechChunk {
            text: "改到下周一上午".into(),
            interim: false,
        });
        install_speech_for_tests(Box::new(Probe {
            network: false,
            available: true,
            capturing: false,
            begins: begins.clone(),
            ends: ends.clone(),
            chunks,
        }));
        let bar = phone_smart_bar().expect("ready mic");
        assert_eq!(
            invoke_label(&bar, OP_IME_VOICE).as_deref(),
            Some(locale::t("ime-voice-ready").as_str())
        );
        assert!(matches!(voice_press(), SpeechStart::Listening));
        assert!(voice_recording());
        assert!(speech_capturing());
        assert_eq!(begins.load(Ordering::SeqCst), 1);
        assert!(poll_voice());
        assert_eq!(composed_text(), "已有改到");
        let live = phone_smart_bar().expect("listening mic");
        assert_eq!(
            invoke_label(&live, OP_IME_VOICE).as_deref(),
            Some(locale::t("ime-voice-live").as_str())
        );
        assert!(poll_voice());
        assert_eq!(composed_text(), "已有改到下周一上午");
        assert_eq!(voice_release(), VoiceRelease::Wrote);
        assert!(!voice_recording());
        assert!(!speech_capturing());
        assert_eq!(ends.load(Ordering::SeqCst), 1);
        assert_eq!(composed_text(), "已有改到下周一上午");
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "已有");

        reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "", false, 4, 640.0);
        let begins = Arc::new(AtomicU32::new(0));
        let ends = Arc::new(AtomicU32::new(0));
        let mut chunks = VecDeque::new();
        chunks.push_back(SpeechChunk {
            text: "打开文件".into(),
            interim: true,
        });
        install_speech_for_tests(Box::new(Probe {
            network: false,
            available: true,
            capturing: false,
            begins: begins.clone(),
            ends: ends.clone(),
            chunks,
        }));
        assert!(matches!(voice_press(), SpeechStart::Listening));
        assert!(poll_voice());
        assert_eq!(voice_release(), VoiceRelease::Wrote);
        assert_eq!(composed_text(), "打开文件");
        assert_eq!(command_utterance().as_deref(), Some("open files"));
        assert!(on_key("Mod+Z"));
        assert!(command_utterance().is_none());

        let begins = Arc::new(AtomicU32::new(0));
        let ends = Arc::new(AtomicU32::new(0));
        install_speech_for_tests(Box::new(Probe {
            network: false,
            available: true,
            capturing: false,
            begins: begins.clone(),
            ends: ends.clone(),
            chunks: VecDeque::new(),
        }));
        assert!(matches!(voice_press(), SpeechStart::Listening));
        assert!(!poll_voice());
        state::clear_ime_field();
        sync_field_presence();
        assert!(!voice_recording());
        assert!(!speech_capturing());
        assert!(ends.load(Ordering::SeqCst) >= 1);

        note("secure", "保持", true, 2, 400.0);
        assert!(phone_smart_bar().is_none());
        assert!(modality_layer().is_none());
        assert!(matches!(
            voice_press(),
            SpeechStart::Off(SpeechOff::SecureField)
        ));
        assert!(!speech_capturing());
        assert_eq!(composed_text(), "保持");
        assert_eq!(voice_release(), VoiceRelease::Idle);

        state::note_shell_viewport(1200.0, 800.0);
        state::clear_ime_field();
        reset_for_tests();
        locale::set_locale("zh-CN");
    }

    fn invoke_label(node: &Component, code: u8) -> Option<String> {
        fn walk(node: &Component, code: u8) -> Option<String> {
            if let ComponentKind::Button { label } = &node.kind
                && let EventAction::Invoke { code: found, .. } = node.on_click
                && found == code
            {
                return Some(label.clone());
            }
            for child in &node.children {
                if let Some(found) = walk(child, code) {
                    return Some(found);
                }
            }
            None
        }
        walk(node, code)
    }

    fn px(dim: Dimension) -> f32 {
        match dim {
            Dimension::Px(value) => value,
            other => panic!("expected px, got {other:?}"),
        }
    }

    fn spacing_px(spacing: Spacing) -> f32 {
        match spacing {
            Spacing::Px(value) => value,
            other => panic!("expected px spacing, got {other:?}"),
        }
    }

    fn candidate_labels(node: &Component) -> Vec<String> {
        let mut labels = Vec::new();
        fn walk(node: &Component, labels: &mut Vec<String>) {
            if let ComponentKind::Button { label } = &node.kind
                && let EventAction::Invoke { code, .. } = node.on_click
                && (super::OP_IME_1..super::OP_IME_1 + 5).contains(&code)
            {
                labels.push(label.clone());
            }
            for child in &node.children {
                walk(child, labels);
            }
        }
        walk(node, &mut labels);
        labels
    }

    #[test]
    fn handwriting_pad_commits_a_pick_and_does_not_learn_the_rest() {
        use w3cos_ime::{HandwritingManifest, InkCandidate, LocalHandwriting};

        struct Probe {
            network: bool,
            available: bool,
            calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
            out: Vec<String>,
        }

        impl LocalHandwriting for Probe {
            fn manifest(&self) -> HandwritingManifest {
                HandwritingManifest {
                    id: "test.local-handwriting".into(),
                    locales: vec!["zh-Hans".into(), "en".into()],
                    network: self.network,
                    available: self.available,
                }
            }

            fn recognize(
                &mut self,
                _width: u32,
                _height: u32,
                _strokes: &[InkStroke],
            ) -> Vec<InkCandidate> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.out
                    .iter()
                    .map(|text| InkCandidate {
                        text: text.clone(),
                        score: 1.0,
                    })
                    .collect()
            }
        }

        reset_for_tests();
        crate::lexicon::reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        note("intent", "已有", false, 4, 480.0);
        let refused = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        install_handwriting_for_tests(Box::new(Probe {
            network: true,
            available: true,
            calls: refused.clone(),
            out: vec!["永".into()],
        }));
        assert_eq!(ink_tap(), Some("ime-toast-ink-net"));
        assert!(ink_pad_layer().is_none());
        assert_eq!(composed_text(), "已有");
        assert_eq!(refused.load(std::sync::atomic::Ordering::SeqCst), 0);

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        install_handwriting_for_tests(Box::new(Probe {
            network: false,
            available: true,
            calls: calls.clone(),
            out: vec!["永".into(), "水".into()],
        }));
        assert_eq!(ink_chip_key(), "ime-ink-ready");
        assert_eq!(ink_tap(), Some("ime-toast-ink-ready"));
        assert_eq!(ink_chip_key(), "ime-ink-live");
        let pad = ink_pad_layer().expect("phone pad");
        let dumped = format!("{pad:?}");
        assert!(dumped.contains("--w3cos-ink"), "{dumped}");
        assert!(dumped.contains(&locale::t("ime-ink-hint")), "{dumped}");
        assert!(phone_keyboard().is_none());
        let geom = with_session(|session| ink_geom(session));
        ink_pointer(0, geom.sx + 12.0, geom.sy + 20.0);
        ink_pointer(1, geom.sx + 80.0, geom.sy + 20.0);
        ink_pointer(2, 0.0, 0.0);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            with_session(|session| session.ink_candidates.clone()),
            vec!["永".to_string(), "水".to_string()]
        );
        assert!(crate::lexicon::freq_of("永").is_none());
        assert!(candidate_layer().is_none());
        let strip = phone_candidate_strip().expect("candidates");
        let labels = candidate_labels(&strip);
        assert!(
            labels.iter().any(|label| label.contains('永')),
            "{labels:?}"
        );
        assert!(commit_candidate(0));
        assert_eq!(composed_text(), "已有永");
        assert_eq!(crate::lexicon::freq_of("永"), Some(1));
        assert_eq!(take_ink_toast(), Some("ime-toast-ink-ok"));
        assert!(with_session(
            |session| session.ink_open && session.ink_strokes.is_empty()
        ));
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "已有");
        assert_eq!(crate::lexicon::freq_of("永"), Some(1));

        ink_pointer(0, geom.sx + 8.0, geom.sy + 8.0);
        ink_pointer(2, 0.0, 0.0);
        assert!(!with_session(|session| session.ink_candidates.is_empty()));
        clear_ink();
        assert!(with_session(|session| session.ink_candidates.is_empty()));
        assert_eq!(crate::lexicon::freq_of("水"), None);
        assert!(on_key("Escape"));
        assert!(ink_pad_layer().is_none());
        assert_eq!(crate::lexicon::freq_of("水"), None);

        state::note_shell_viewport(1200.0, 800.0);
        note("intent", "已有", false, 4, 180.0);
        assert_eq!(ink_tap(), Some("ime-toast-ink-ready"));
        let desk = ink_pad_layer().expect("desktop pad");
        let dumped = format!("{desk:?}");
        assert!(dumped.contains("--w3cos-ink"), "{dumped}");
        assert!(dumped.contains(&locale::t("ime-ink-close")), "{dumped}");
        assert!(on_key("Escape"));
        assert!(ink_pad_layer().is_none());

        state::note_shell_viewport(390.0, 760.0);
        note("secure", "", true, 9, 480.0);
        assert_eq!(ink_tap(), Some("ime-toast-ink-secure"));
        assert!(ink_pad_layer().is_none());
        assert!(phone_smart_bar().is_none());

        state::note_shell_viewport(1200.0, 800.0);
        reset_for_tests();
        locale::set_locale("en");
    }

    #[test]
    fn screen_text_asks_before_one_region_and_learns_only_a_pick() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use w3cos_ime::{LocalScreenText, ScreenSnippet, ScreenTextManifest};

        struct Probe {
            network: bool,
            available: bool,
            calls: std::sync::Arc<AtomicU32>,
            out: Vec<String>,
        }

        impl LocalScreenText for Probe {
            fn manifest(&self) -> ScreenTextManifest {
                ScreenTextManifest {
                    id: "test.local-screen-text".into(),
                    locales: vec!["zh-Hans".into(), "en".into()],
                    network: self.network,
                    available: self.available,
                }
            }

            fn recognize(
                &mut self,
                _image: &ScreenImage,
                _region: ScreenRegion,
            ) -> Vec<ScreenSnippet> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.out
                    .iter()
                    .map(|text| ScreenSnippet {
                        text: text.clone(),
                        score: 1.0,
                    })
                    .collect()
            }
        }

        reset_for_tests();
        crate::lexicon::reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        note("intent", "已有", false, 4, 180.0);
        let refused = std::sync::Arc::new(AtomicU32::new(0));
        install_screen_for_tests(Box::new(Probe {
            network: true,
            available: true,
            calls: refused.clone(),
            out: vec!["周五".into()],
        }));
        assert_eq!(screen_tap(), Some("ime-toast-screen-net"));
        assert!(screen_layer().is_none());
        assert_eq!(refused.load(Ordering::SeqCst), 0);
        assert!(take_screen_decision().is_none());

        let calls = std::sync::Arc::new(AtomicU32::new(0));
        install_screen_for_tests(Box::new(Probe {
            network: false,
            available: true,
            calls: calls.clone(),
            out: vec!["周五".into(), "评审".into()],
        }));
        assert_eq!(screen_chip_key(), "ime-screen-ready");
        assert_eq!(screen_tap(), Some("ime-toast-screen-ask"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let card = screen_layer().expect("permission card");
        let dumped = format!("{card:?}");
        assert!(dumped.contains("--w3cos-screen"), "{dumped}");
        assert!(dumped.contains(&locale::t("ime-screen-title")), "{dumped}");
        assert!(dumped.contains(&locale::t("confirm")), "{dumped}");
        assert_eq!(take_screen_decision(), None);
        assert_eq!(screen_cancel(), Some("ime-toast-screen-cancel"));
        assert_eq!(take_screen_decision(), Some(ScreenDecision::Cancelled));
        assert!(screen_layer().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(crate::lexicon::freq_of("周五").is_none());

        assert_eq!(screen_tap(), Some("ime-toast-screen-ask"));
        assert_eq!(screen_allow(), Some("ime-toast-screen-allow"));
        assert_eq!(take_screen_decision(), Some(ScreenDecision::Allowed));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let overlay = screen_layer().expect("region indicator");
        let dumped = format!("{overlay:?}");
        assert!(dumped.contains(&locale::t("ime-screen-hint")), "{dumped}");
        assert!(dumped.contains("--w3cos-ink"), "{dumped}");
        install_screen_pixels_for_tests(ScreenImage {
            width: 120,
            height: 80,
            rgba: vec![255; 120 * 80 * 4],
        });
        screen_pointer(0, 200.0, 160.0);
        screen_pointer(1, 800.0, 420.0);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "drag does not capture");
        screen_pointer(2, 800.0, 420.0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            screen_layer().is_none(),
            "the indicator leaves after the read"
        );
        assert_eq!(
            with_session(|session| session.screen_candidates.clone()),
            vec!["周五".to_string(), "评审".to_string()]
        );
        assert!(crate::lexicon::freq_of("周五").is_none());
        assert!(crate::lexicon::freq_of("评审").is_none());
        assert_eq!(take_screen_toast(), Some("ime-toast-screen-ready"));
        assert!(commit_candidate(0));
        assert_eq!(composed_text(), "已有周五");
        assert_eq!(crate::lexicon::freq_of("周五"), Some(1));
        assert!(crate::lexicon::freq_of("评审").is_none());
        assert_eq!(take_screen_toast(), Some("ime-toast-screen-ok"));
        assert!(on_key("Mod+Z"));
        assert_eq!(composed_text(), "已有");
        assert_eq!(crate::lexicon::freq_of("周五"), Some(1));

        assert_eq!(screen_tap(), Some("ime-toast-screen-ask"));
        assert_eq!(screen_allow(), Some("ime-toast-screen-allow"));
        let _ = take_screen_decision();
        screen_pointer(0, 220.0, 180.0);
        screen_pointer(2, 226.0, 184.0);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a tiny region is not read");
        assert_eq!(take_screen_toast(), Some("ime-toast-screen-small"));
        assert_eq!(screen_cancel(), Some("ime-toast-screen-cancel"));
        assert_eq!(take_screen_decision(), Some(ScreenDecision::Cancelled));

        note("secure", "", true, 9, 180.0);
        assert_eq!(screen_tap(), Some("ime-toast-screen-secure"));
        assert!(screen_layer().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        state::note_shell_viewport(1200.0, 800.0);
        reset_for_tests();
        locale::set_locale("en");
    }
}
