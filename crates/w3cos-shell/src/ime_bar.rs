//! Shell pinyin session.
//!
//! The spelling, committed base, and undo stack live here. `w3cos-ime` only
//! looks the spelling up. Secure fields and direct (EN) mode never call the
//! engine. L2 is not consulted: the lookup hub is constructed with L2 off.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_ime::{ImeHub, InputIntent, Modality, PinyinEngine, Proposal, Purpose};
use w3cos_runtime::state::{self, ImeFieldNote};
use w3cos_std::Component;
use w3cos_std::color::Color;
use w3cos_std::style::*;

use super::{
    CHROME_Z, OP_IME_1, OP_IME_MODE, OP_SOFTKEY, ink_token, invoke, line_strong, touch_min,
};
use crate::locale;

/// Room left above the bottom edge for the phone keyboard that sits under
/// the candidate bar.
pub const PHONE_KEYBOARD_RESERVE: f32 = 240.0;
/// Key rows plus padding. The phone stack leaves this much room above the
/// home indicator so the absolute keyboard does not cover the intent field.
pub const KEYBOARD_HEIGHT: f32 = 218.0;
const BAR_BLOCK: f32 = 120.0;
const KEY_ROW: f32 = 48.0;

struct Caret {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

struct Session {
    /// `Some(true)` forces pinyin. `None` follows the active locale.
    override_pinyin: Option<bool>,
    spelling: String,
    base: String,
    role: String,
    index: usize,
    secure: bool,
    has_field: bool,
    undo: Vec<String>,
    caret: Caret,
    candidates: Vec<String>,
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

fn lookup(spelling: &str) -> Vec<String> {
    let mut engine = PinyinEngine::new();
    let hub = ImeHub::with_l2(false);
    let session = hub.begin_session("shell", Purpose::Text, vec!["zh-Hans".into()]);
    let intent = InputIntent::text(Modality::Key, spelling, &["zh-Hans"]);
    hub.handle_intent(&session, &intent, &mut [&mut engine])
        .into_iter()
        .filter_map(|proposal| match proposal {
            Proposal::Candidate { text, .. } => Some(text),
            _ => None,
        })
        .collect()
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

fn refresh(session: &mut Session) {
    session.candidates = if session.spelling.is_empty() || session.secure || !pinyin_mode(session) {
        Vec::new()
    } else {
        lookup(&session.spelling)
    };
}

fn commit_at(session: &mut Session, index: usize) -> bool {
    if session.spelling.is_empty() {
        return false;
    }
    let Some(chunk) = session.candidates.get(index).cloned().or_else(|| {
        if index == 0 {
            Some(session.spelling.clone())
        } else {
            None
        }
    }) else {
        return false;
    };
    session.base.push_str(&chunk);
    session.undo.push(chunk);
    session.spelling.clear();
    session.candidates.clear();
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
    queue(session, session.base.clone());
    true
}

fn adopt(session: &mut Session, note: &ImeFieldNote) {
    if session.has_field && note.index != session.index && !session.spelling.is_empty() {
        session.spelling.clear();
        session.candidates.clear();
        queue(session, session.base.clone());
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
}

fn is_letter(name: &str) -> bool {
    name.len() == 1 && name.as_bytes()[0].is_ascii_lowercase()
}

/// Consume a shell key. `true` means the runtime must not also insert it.
pub(crate) fn on_key(name: &str) -> bool {
    with_session(|session| {
        if name == "Mod+Space" {
            session.override_pinyin = Some(!pinyin_mode(session));
            if !session.spelling.is_empty() {
                session.spelling.clear();
                session.candidates.clear();
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
            session.spelling.clear();
            session.candidates.clear();
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
                    if session.spelling.is_empty() {
                        session.candidates.clear();
                        queue(session, session.base.clone());
                    } else {
                        refresh(session);
                        queue(session, format!("{}{}", session.base, session.spelling));
                    }
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
                let index = (name.as_bytes()[0] - b'1') as usize;
                let _ = commit_at(session, index);
                true
            }
            letter if is_letter(letter) => {
                session.spelling.push_str(letter);
                refresh(session);
                queue(session, format!("{}{}", session.base, session.spelling));
                true
            }
            _ => false,
        }
    })
}

pub(crate) fn toggle_mode() {
    let _ = on_key("Mod+Space");
}

pub(crate) fn commit_candidate(index: usize) -> bool {
    with_session(|session| {
        if session.secure || !pinyin_mode(session) {
            return false;
        }
        commit_at(session, index)
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

fn place(width: f32, height: f32, caret: &Caret) -> (f32, f32) {
    if width < 600.0 {
        let y = (height - PHONE_KEYBOARD_RESERVE - 8.0 - BAR_BLOCK).max(8.0);
        return (8.0, y);
    }
    let x = if caret.w > 1.0 {
        caret.x.max(8.0).min((width - 360.0).max(8.0))
    } else {
        16.0
    };
    let y = if caret.h > 1.0 {
        let below = caret.y + caret.h + 6.0;
        if below + 48.0 < height - 88.0 {
            below
        } else {
            (caret.y - 48.0 - 6.0).max(8.0)
        }
    } else {
        let intent_top = (height - 84.0 - 78.0).max(8.0);
        (intent_top - 8.0 - 48.0).max(8.0)
    };
    (x, y)
}

/// Drop an uncommitted spelling when focus has left every text field.
pub(crate) fn sync_field_presence() {
    if state::current_ime_field().is_some() {
        return;
    }
    with_session(|session| {
        session.spelling.clear();
        session.candidates.clear();
        session.has_field = false;
    });
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
                queue(session, session.base.clone());
                SoftOutcome::Edit
            }
            other if other.chars().count() == 1 && !other.chars().any(|ch| ch.is_control()) => {
                session.base.push_str(other);
                queue(session, session.base.clone());
                SoftOutcome::Edit
            }
            _ => SoftOutcome::None,
        }
    })
}

fn candidates_visible(session: &mut Session) -> bool {
    pinyin_mode(session) && !session.secure && !session.spelling.is_empty()
}

fn candidate_children() -> Vec<Component> {
    let (spelling, candidates) =
        with_session(|session| (session.spelling.clone(), session.candidates.clone()));
    let mut children = vec![
        mode_button(),
        Component::text(
            locale::t_args("ime-preedit", &[("spelling", &spelling)]),
            Style {
                font_size: 14.0,
                font_weight: 650,
                color: ink_token(),
                ..Style::default()
            },
        ),
    ];
    for (index, text) in candidates.iter().take(9).enumerate() {
        let n = (index + 1).to_string();
        let label = locale::t_args("ime-candidate", &[("n", &n), ("text", text)]);
        children.push(candidate_button(label, OP_IME_1 + index as u8));
    }
    children
}

pub(crate) fn candidate_layer() -> Option<Component> {
    let (width, _) = state::shell_viewport();
    if width < 600.0 {
        return None;
    }
    let visible = with_session(candidates_visible);
    if !visible {
        return None;
    }
    let (width, height) = state::shell_viewport();
    let (x, y) = candidate_origin(width, height);
    let bar_w = (width - 16.0).clamp(160.0, 640.0);
    let mut bar = Component::row(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(x),
            top: Dimension::Px(y),
            width: Dimension::Px(bar_w),
            gap: 6.0,
            padding: Edges::all(8.0),
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::Center,
            background: Color::from_hex("#162433"),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 12.0,
            z_index: CHROME_Z + 5,
            ..Style::default()
        },
        candidate_children(),
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
            gap: 6.0,
            margin: Edges::xy(8.0, 4.0),
            padding: Edges::all(8.0),
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::Center,
            background: Color::from_hex("#162433"),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 12.0,
            ..Style::default()
        },
        candidate_children(),
    ))
}

fn keep_ime(style: &mut Style) {
    let mut props = style.custom_properties.take().unwrap_or_default();
    props.insert("--w3cos-ime-keep".to_string(), "1".to_string());
    style.custom_properties = Some(props);
}

fn soft_key(label: String, key: &str, grow: f32, special: bool) -> Component {
    let mut style = Style {
        flex_grow: grow,
        flex_shrink: 1.0,
        height: Dimension::Px(KEY_ROW),
        min_height: Dimension::Px(KEY_ROW),
        background: Color::from_hex(if special { "#1b2c3e" } else { "#24384a" }),
        border_radius: 8.0,
        font_size: 16.0,
        font_weight: 650,
        color: Color::from_hex("#f4f7fb"),
        padding: Edges::ZERO,
        box_sizing: BoxSizing::BorderBox,
        ..Style::default()
    };
    style.custom_properties = Some(HashMap::from([
        ("--w3cos-softkey".to_string(), key.to_string()),
        ("--w3cos-ime-keep".to_string(), "1".to_string()),
    ]));
    Component::button_with_click(label, style, invoke(OP_SOFTKEY))
}

fn letter_row(letters: &[&str]) -> Component {
    Component::row(
        Style {
            height: Dimension::Px(KEY_ROW),
            gap: 4.0,
            align_items: AlignItems::Center,
            flex_shrink: 0.0,
            ..Style::default()
        },
        letters
            .iter()
            .map(|letter| soft_key((*letter).to_string(), letter, 1.0, false))
            .collect(),
    )
}

/// Basic QWERTY keyboard for the 390×760 phone posture, including the pinyin toggle.
pub(crate) fn phone_keyboard() -> Option<Component> {
    if !keyboard_open() {
        return None;
    }
    let (width, height) = state::shell_viewport();
    let mut board = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(0.0),
            top: Dimension::Px((height - KEYBOARD_HEIGHT).max(0.0)),
            width: Dimension::Px(width),
            height: Dimension::Px(KEYBOARD_HEIGHT),
            padding: Edges::xy(4.0, 4.0),
            gap: 6.0,
            justify_content: JustifyContent::Center,
            background: Color::from_hex("#10161d"),
            border_width: 1.0,
            border_color: line_strong(),
            z_index: CHROME_Z + 6,
            flex_shrink: 0.0,
            ..Style::default()
        },
        vec![
            letter_row(&["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"]),
            letter_row(&["a", "s", "d", "f", "g", "h", "j", "k", "l"]),
            Component::row(
                Style {
                    height: Dimension::Px(KEY_ROW),
                    gap: 4.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
                vec![
                    {
                        let mut key = mode_button();
                        key.style.flex_grow = 1.4;
                        key.style.height = Dimension::Px(KEY_ROW);
                        key.style.min_height = Dimension::Px(KEY_ROW);
                        key
                    },
                    soft_key("z".into(), "z", 1.0, false),
                    soft_key("x".into(), "x", 1.0, false),
                    soft_key("c".into(), "c", 1.0, false),
                    soft_key("v".into(), "v", 1.0, false),
                    soft_key("b".into(), "b", 1.0, false),
                    soft_key("n".into(), "n", 1.0, false),
                    soft_key("m".into(), "m", 1.0, false),
                    soft_key(locale::t("kbd-backspace"), "Backspace", 1.4, true),
                ],
            ),
            Component::row(
                Style {
                    height: Dimension::Px(KEY_ROW),
                    gap: 4.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
                vec![
                    soft_key(locale::t("kbd-space"), "Space", 4.0, true),
                    soft_key(locale::t("kbd-enter"), "Enter", 1.6, true),
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
        padding: Edges::xy(10.0, 0.0),
        height: Dimension::Px(touch_min()),
        min_height: Dimension::Px(touch_min()),
        box_sizing: BoxSizing::BorderBox,
        ..Style::default()
    };
    keep_ime(&mut style);
    Component::button_with_click(label, style, invoke(OP_IME_MODE))
}

fn candidate_button(label: String, code: u8) -> Component {
    let mut style = Style {
        background: Color::from_hex("#1c8f62"),
        border_radius: 8.0,
        font_size: 14.0,
        font_weight: 650,
        color: Color::WHITE,
        padding: Edges::xy(8.0, 0.0),
        height: Dimension::Px(touch_min()),
        min_height: Dimension::Px(touch_min()),
        box_sizing: BoxSizing::BorderBox,
        ..Style::default()
    };
    keep_ime(&mut style);
    Component::button_with_click(label, style, invoke(code))
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
        assert!(y + BAR_BLOCK <= 760.0 - PHONE_KEYBOARD_RESERVE + 0.5);
        let (_, desktop_y) = candidate_origin(1200.0, 800.0);
        assert!((desktop_y - (200.0 + 36.0 + 6.0)).abs() < 0.5);
        note("intent", "ni", false, 4, 740.0);
        assert!(on_key("h"));
        let (_, above) = candidate_origin(1200.0, 800.0);
        assert!(above < 740.0);
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
        assert!(format!("{strip:?}").contains("你好"));
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
    }
}
