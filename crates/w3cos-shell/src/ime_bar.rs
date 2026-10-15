//! Shell pinyin session.
//!
//! The spelling, committed base, and undo stack live here. `w3cos-ime` only
//! looks the spelling up. Secure fields and direct (EN) mode never call the
//! engine. L2 is not consulted: the lookup hub is constructed with L2 off.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_ime::{
    ImeHub, InputIntent, IntentHint, IntentPayload, Modality, PinyinEngine, Proposal, Purpose,
};
use w3cos_runtime::state::{self, ImeFieldNote};
use w3cos_std::Component;
use w3cos_std::color::Color;
use w3cos_std::style::*;

use super::{
    CHROME_Z, OP_IME_1, OP_IME_ACTION, OP_IME_MODE, OP_IME_PAGE_NEXT, OP_IME_PAGE_PREV, OP_SOFTKEY,
    ink_token, invoke, line_strong,
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
        }
    }
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
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
    if session.actions.is_empty() {
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
        if name == "Escape" {
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
        let pages = session
            .candidates
            .len()
            .div_ceil(text_slots(session))
            .max(1);
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
    with_session(|session| format!("{}{}", session.base, session.spelling))
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
    let composing = pinyin_mode(session) && !session.spelling.is_empty();
    composing || !session.actions.is_empty()
}

fn candidate_children(show_preedit: bool) -> Vec<Component> {
    let (spelling, preedit, candidates, page, actions) = with_session(|session| {
        (
            session.spelling.clone(),
            session.preedit.clone(),
            session.candidates.clone(),
            session.page,
            session.actions.clone(),
        )
    });
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
    style.custom_properties = Some(HashMap::from([
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
    if !keyboard_open() {
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
pub(crate) fn reset_for_tests() {
    SESSION.with(|slot| *slot.borrow_mut() = Session::default());
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
}
