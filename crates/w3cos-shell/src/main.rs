//! W3C OS Desktop Shell — the system-level GUI.
//!
//! Boot sequence: Linux kernel → init → S99w3cos → w3cos-shell (this binary)
//! Click an app icon → a floating window opens over the desktop.
//! Drag the title bar to move it; drag an edge or corner to resize.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

mod agent_host;
mod gesture;
mod handoff;
mod hpke;
mod ime_bar;
mod lexicon;
mod locale;
mod windows;

#[cfg(feature = "desktop")]
use crate::state;
use w3cos_ai_bridge::dom_access::ActionType;
use w3cos_ai_bridge::identity::LocalIdentityStub;
use w3cos_ai_bridge::intent::{
    Decision, ExecuteResult, IntentSession, IntentTier, ProposeResult, ShellApp, ShellCommand,
    ShellContext,
};
use w3cos_ai_bridge::permissions::{AgentPermissions, BridgeOp};
use w3cos_ai_bridge::shell_tools::{self, ShellView};
use w3cos_runtime::capability_gate;
#[cfg(not(feature = "desktop"))]
use w3cos_runtime::state;
use w3cos_std::color::Color;
use w3cos_std::style::*;
use w3cos_std::{Component, ComponentKind, EventAction, Style};

mod shared_launch;
mod sign_host;

const APP_SIGNAL: usize = 0;
const NIGHT_SIGNAL: usize = 1;
const PHASE_SIGNAL: usize = 2;
const CONTEXT_SIGNAL: usize = 3;
const UNLOCKED_SIGNAL: usize = 4;
const TASKS_SIGNAL: usize = 10;
const DRAFT_SIGNAL: usize = 5;
const SUMMARY_SIGNAL: usize = 6;
const IMPACT_SIGNAL: usize = 7;
const NOTICE_SIGNAL: usize = 8;
const RECEIPT_SIGNAL: usize = 9;
const STORAGE_SIGNAL: usize = 11;
const SUBJECT_SIGNAL: usize = 12;
const ENROLL_SIGNAL: usize = 13;
const TIER_SIGNAL: usize = 14;
const STEPS_SIGNAL: usize = 15;
const TERMINAL_SIGNAL: usize = 16;
const TERM_OUT_SIGNAL: usize = 17;
const OPEN_MASK_SIGNAL: usize = 18;
const MINIMIZED_MASK_SIGNAL: usize = 19;
const LAUNCHER_SIGNAL: usize = 20;
const LAUNCHER_QUERY_SIGNAL: usize = 21;
const SETTINGS_TAB_SIGNAL: usize = 22;
const FILES_FOLDER_SIGNAL: usize = 23;
const TOAST_SIGNAL: usize = 24;
const RECEIPTS_OPEN_SIGNAL: usize = 25;
const PERM_SIGNAL: usize = 26;
const RECEIPT_PAGE_SIGNAL: usize = 27;
const NOTIFY_SIGNAL: usize = 28;
const NOTIFY_UNREAD_SIGNAL: usize = 29;
const RECEIPT_FILTER_SIGNAL: usize = 30;
const SWITCHER_SIGNAL: usize = 31;
const FILE_PREVIEW_SIGNAL: usize = 32;
const RECEIPT_QUERY_SIGNAL: usize = 33;
const LOCK_PASSPHRASE_SIGNAL: usize = 34;
const LEXICON_DRAFT_SIGNAL: usize = 35;
const CONFIRM_FOCUS_SIGNAL: usize = 36;
const SETTINGS_MENU_SIGNAL: usize = 37;
const PENDING_LOCALE_SIGNAL: usize = 38;
const SIGN_SIGNAL: usize = 39;
const SIGN_MESSAGE_SIGNAL: usize = 40;
const SIGN_SUMMARY_SIGNAL: usize = 41;
const SIGN_IMPACT_SIGNAL: usize = 42;
const TOOL_SIGNAL: usize = 43;
const HOST_SIGNAL: usize = 44;
const CHAIN_RPC_SIGNAL: usize = 45;
const CHAIN_SIGNAL: usize = 46;
const BROADCAST_SIGNAL: usize = 47;
const SANDBOX_SIGNAL: usize = 48;
const SANDBOX_FS_SIGNAL: usize = 49;
const SANDBOX_NET_SIGNAL: usize = 50;

const OP_PROPOSE: u8 = 1;
const OP_CONFIRM: u8 = 2;
const OP_CANCEL: u8 = 3;
const OP_REVOKE: u8 = 4;
const OP_RESTORE: u8 = 5;
const OP_LOCK: u8 = 6;
const OP_UNLOCK: u8 = 7;
const OP_CHIP_FILES: u8 = 10;
const OP_CHIP_NIGHT: u8 = 11;
const OP_CHIP_APPS: u8 = 12;
const OP_CHIP_CONTEXT: u8 = 13;
const OP_TOGGLE_TASKS: u8 = 14;
const OP_CHIP_TERMINAL: u8 = 15;
const OP_CHIP_SETTINGS: u8 = 16;
const OP_ENROLL: u8 = 17;
const OP_SHARED_APP: u8 = 18;
const OP_SHARED_PAGE: u8 = 19;
const OP_SHARED_PLUGIN: u8 = 20;
const OP_RUN_TERMINAL: u8 = 21;
const OP_MINIMIZE: u8 = 22;
const OP_CLOSE: u8 = 23;
const OP_CYCLE: u8 = 24;
const OP_LAUNCHER: u8 = 25;
const OP_LAUNCHER_SEARCH: u8 = 26;
const OP_DISMISS_TOAST: u8 = 27;
const OP_SETTINGS_LOCAL: u8 = 28;
const OP_SETTINGS_IDENTITY: u8 = 29;
const OP_SETTINGS_BRIDGE: u8 = 30;
const OP_SETTINGS_L2: u8 = 31;
const OP_L2_TOGGLE: u8 = 32;
const OP_FILES_HOME: u8 = 33;
const OP_FILES_DOCS: u8 = 34;
const OP_FILES_PICS: u8 = 35;
const OP_FILES_DOWN: u8 = 36;
const OP_FILES_MUSIC: u8 = 37;
const OP_FILES_PROJECTS: u8 = 38;
const OP_FILES_W3COS: u8 = 39;
const OP_TOGGLE_RECEIPTS: u8 = 40;
const OP_FOCUS_DESKTOP: u8 = 41;
const OP_FOCUS_FILES: u8 = 42;
const OP_FOCUS_TERMINAL: u8 = 43;
const OP_FOCUS_SETTINGS: u8 = 44;
const OP_FOCUS_AGENT: u8 = 45;
const OP_FOCUS_BROWSER: u8 = 46;
const OP_FOCUS_EDITOR: u8 = 47;
const OP_MAXIMIZE: u8 = 48;
const OP_LOCALE_ZH: u8 = 49;
const OP_LOCALE_EN: u8 = 50;
const OP_PERM_ONCE: u8 = 51;
const OP_PERM_DENY: u8 = 52;
const OP_NOTIFY_TOGGLE: u8 = 53;
const OP_RECEIPT_PREV: u8 = 54;
const OP_RECEIPT_NEXT: u8 = 55;
const OP_TRAY_L2: u8 = 56;
const OP_SCRIM: u8 = 57;
const OP_CLOCK: u8 = 58;
const OP_FILTER_ALL: u8 = 59;
const OP_FILTER_ALLOWED: u8 = 60;
const OP_FILTER_DENIED: u8 = 61;
const OP_FILTER_CANCELLED: u8 = 62;
const OP_COPY_SUBJECT: u8 = 63;
const OP_PREVIEW: u8 = 64;
const OP_SWITCHER: u8 = 65;
const OP_SETTINGS_DEVICES: u8 = 66;
const OP_PAIR_NO: u8 = 67;
const OP_PAIR_YES: u8 = 68;
const OP_HANDOFF_LATER: u8 = 69;
const OP_HANDOFF_GO: u8 = 70;
const OP_HANDOFF_OPEN: u8 = 71;
const OP_HANDOFF_HIDE: u8 = 72;
const OP_SYNC_RECEIPTS: u8 = 73;
const OP_SYNC_DRAFTS: u8 = 74;
const OP_RECEIPT_QUERY: u8 = 75;
const OP_SYNC_CLIPBOARD: u8 = 76;
const OP_IME_1: u8 = 77;
const OP_IME_2: u8 = 78;
const OP_IME_3: u8 = 79;
const OP_IME_4: u8 = 80;
const OP_IME_5: u8 = 81;
const OP_IME_6: u8 = 82;
const OP_IME_7: u8 = 83;
const OP_IME_8: u8 = 84;
const OP_IME_9: u8 = 85;
const OP_IME_MODE: u8 = 86;
const OP_SOFTKEY: u8 = 87;
const OP_IME_PAGE_PREV: u8 = 88;
const OP_IME_PAGE_NEXT: u8 = 89;
const OP_IME_ACTION: u8 = 90;
const OP_SYNC_ALLOW: u8 = 91;
const OP_SYNC_SKIP: u8 = 92;
const OP_HANDOFF_CANCEL: u8 = 93;
const OP_HANDOFF_REVEAL: u8 = 94;
const OP_CLIP_MINE: u8 = 95;
const OP_CLIP_THEIRS: u8 = 96;
const OP_SETTINGS_LEXICON: u8 = 97;
const OP_LEXICON_STEALTH: u8 = 98;
const OP_LEXICON_APP: u8 = 99;
const OP_LEXICON_SAVE: u8 = 100;
const OP_LEXICON_DROP: u8 = 101;
const OP_LEXICON_EXPORT: u8 = 102;
const OP_LEXICON_CLEAR: u8 = 103;
const OP_SETTINGS_LANGUAGE: u8 = 104;
const OP_SETTINGS_BACK: u8 = 105;
const OP_NOTIFY_OPEN: u8 = 106;
const OP_IME_VOICE: u8 = 107;
const OP_IME_INK: u8 = 108;
const OP_IME_CLIPBOARD: u8 = 109;
const OP_IME_SCREEN: u8 = 110;
const OP_PAIR_IMPORT: u8 = 111;
const OP_IME_MODEL: u8 = 112;
const OP_IME_INK_CLEAR: u8 = 113;
const OP_IME_INK_MODEL: u8 = 114;
const OP_IME_SCREEN_YES: u8 = 115;
const OP_IME_SCREEN_NO: u8 = 116;
const OP_IME_SCREEN_MODEL: u8 = 117;
const OP_SIGN_PROPOSE: u8 = 118;
const OP_SIGN_ONCE: u8 = 119;
const OP_SIGN_DENY: u8 = 120;
const OP_CHAIN_ONCE: u8 = 121;
const OP_CHAIN_DENY: u8 = 122;
const OP_CHAIN_SAVE: u8 = 123;
const OP_TX_SEND: u8 = 124;
const OP_TX_HOLD: u8 = 125;
const OP_INFER_BACKEND: u8 = 126;
const OP_SANDBOX_FS: u8 = 127;
const OP_SANDBOX_NET: u8 = 128;

fn main() {
    shell_main();
}

pub(crate) fn shell_main() {
    if shared_launch::launch_from_args() {
        return;
    }
    locale::apply_startup();
    eprintln!("W3C OS Desktop Shell v0.1.0 — starting...");

    let mut ai_port = None;
    let mut ai_token = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ai-port" => ai_port = args.next(),
            "--ai-token" => ai_token = args.next(),
            _ => {}
        }
    }
    if let Some(token) = ai_token.filter(|token| !token.is_empty()) {
        // The process is still single-threaded here. The bridge reads this
        // when the window starts the loopback listener.
        unsafe { std::env::set_var("W3COS_AI_TOKEN", token) };
    }
    if let Some(port_str) = ai_port
        && let Ok(port) = port_str.parse::<u16>()
    {
        #[cfg(feature = "desktop")]
        crate::enable_ai_bridge(port);
        #[cfg(not(feature = "desktop"))]
        w3cos_runtime::enable_ai_bridge(port);
        eprintln!("[AI Bridge] Will start on port {port}");
    }

    state::set_invoke_hook(on_shell_intent);
    state::set_shell_drag_hook(on_window_drag);
    state::set_shell_swipe_hook(on_shell_swipe);
    state::set_shell_key_hook(on_shell_key);
    state::set_agent_poll_hook(on_agent_poll);
    install_ime_hooks();
    ensure_clock_timer();
    handoff::ensure_listener();
    agent_host::install_wake();
    w3cos_ai_bridge::server::set_intent_hooks(bridge_propose, bridge_confirm);
    w3cos_ai_bridge::server::set_mcp_hook(bridge_mcp_propose);
    w3cos_ai_bridge::server::set_host_hook(bridge_host_propose);
    #[cfg(feature = "desktop")]
    let launched = crate::run_app(build_shell);
    #[cfg(not(feature = "desktop"))]
    let launched = w3cos_runtime::run_app(build_shell);
    launched.expect("W3C OS Shell crashed");
}

fn bridge_propose(utterance: &str) -> String {
    on_shell_intent(OP_PROPOSE, utterance);
    let summary = state::get_signal_text(SUMMARY_SIGNAL);
    if !summary.is_empty() {
        return summary;
    }
    state::get_signal_text(NOTICE_SIGNAL)
}

fn bridge_confirm() -> String {
    on_shell_intent(OP_CONFIRM, "");
    let notice = state::get_signal_text(NOTICE_SIGNAL);
    if !notice.is_empty() {
        return notice;
    }
    state::get_signal_text(SUMMARY_SIGNAL)
}

fn bridge_mcp_propose(name: &str) {
    show_tool_confirm(name);
    if state::get_signal(UNLOCKED_SIGNAL) != 0 {
        state::request_focus_label(&locale::t("confirm"));
    }
}

fn bridge_host_propose(name: &str, summary: &str, impact: &str, message: &str) {
    ensure_shell_signals();
    if name == "wallet_sign" || name == "wallet_sign_tx" {
        let shown = if name == "wallet_sign_tx" {
            w3cos_ai_bridge::tx::preview_from_json(&locale::current(), message)
                .unwrap_or_else(|| message.to_string())
        } else {
            message.to_string()
        };
        state::set_text_signal(SIGN_MESSAGE_SIGNAL, &shown);
        state::set_text_signal(SIGN_SUMMARY_SIGNAL, summary);
        state::set_text_signal(SIGN_IMPACT_SIGNAL, impact);
        state::set_signal(SIGN_SIGNAL, 1);
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
        if state::get_signal(UNLOCKED_SIGNAL) != 0 {
            state::request_focus_label(&locale::t("sign-deny"));
        }
        return;
    }
    if name.starts_with("chain_") {
        state::set_signal(CHAIN_SIGNAL, 1);
        state::set_signal(HOST_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_text_signal(SUMMARY_SIGNAL, summary);
        state::set_text_signal(IMPACT_SIGNAL, impact);
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
        if state::get_signal(UNLOCKED_SIGNAL) != 0 {
            state::request_focus_label(&locale::t("chain-deny"));
        }
        return;
    }
    state::set_signal(HOST_SIGNAL, 1);
    state::set_signal(PHASE_SIGNAL, 1);
    state::set_signal(TOOL_SIGNAL, 0);
    state::set_text_signal(TIER_SIGNAL, "L0");
    state::set_text_signal(SUMMARY_SIGNAL, summary);
    state::set_text_signal(IMPACT_SIGNAL, impact);
    state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "confirm");
    if state::get_signal(UNLOCKED_SIGNAL) != 0 {
        state::request_focus_label(&locale::t("confirm"));
    }
}

fn show_chain_read() {
    ensure_shell_signals();
    state::set_text_signal(CHAIN_RPC_SIGNAL, "http://127.0.0.1:8545");
    bridge_host_propose(
        "chain_balance",
        &locale::t("chain-balance-summary"),
        &locale::t("chain-balance-impact"),
        "",
    );
}

fn show_host_confirm() {
    bridge_host_propose(
        "device_did",
        &locale::t("device-did-summary"),
        &locale::t("device-did-impact"),
        "",
    );
}

fn apply_host_mail(session: &mut IntentSession) {
    let Some(ticket) = w3cos_ai_bridge::tool_host::take_unapplied() else {
        return;
    };
    if let Err(text) = sign_host::propose_tool(
        &ticket.name,
        &ticket.message,
        session.identity_unlocked(),
        &locale::current(),
        now_ms(),
        session.signed_high_water().saturating_add(1),
    ) {
        w3cos_ai_bridge::tool_host::fail_host(&text);
        state::set_signal(HOST_SIGNAL, 0);
        state::set_signal(SIGN_SIGNAL, 0);
        sync_session(session, state::get_signal(PHASE_SIGNAL), &text);
    }
}

fn publish_host_receipt(receipt: &w3cos_ai_bridge::tool_host::SignedReceipt) {
    w3cos_ai_bridge::tool_host::publish_host_receipt(receipt);
}

fn show_tool_confirm(name: &str) {
    ensure_shell_signals();
    state::set_signal(PHASE_SIGNAL, 1);
    state::set_signal(TOOL_SIGNAL, 1);
    state::set_text_signal(TIER_SIGNAL, "L2");
    state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "confirm");
    state::set_text_signal(
        SUMMARY_SIGNAL,
        locale::t_args("mcp-tool-summary", &[("name", name)]),
    );
    state::set_text_signal(IMPACT_SIGNAL, locale::t("mcp-tool-impact"));
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShellNotice {
    title: String,
    detail: String,
    /// Passed back as the click draft: `receipt:#N`, `devices`, `incoming`, or empty.
    draft: String,
}

thread_local! {
    static SESSION: RefCell<Option<IntentSession>> = const { RefCell::new(None) };
    static SESSION_BOOTED: Cell<bool> = const { Cell::new(false) };
    static NOTICES: RefCell<Vec<ShellNotice>> = const { RefCell::new(Vec::new()) };
    static SEEN_RECEIPTS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static RECEIPT_NOTICES_BASELINED: Cell<bool> = const { Cell::new(false) };
    static NOTICE_FOCUS: RefCell<String> = const { RefCell::new(String::new()) };
    static LAST_CLOCK: RefCell<String> = const { RefCell::new(String::new()) };
    static COPIED_SUBJECT: RefCell<String> = const { RefCell::new(String::new()) };
    static CLOCK_ARMED: Cell<bool> = const { Cell::new(false) };
}

fn ensure_shell_signals() {
    state::ensure_signals(|| {
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(1);
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("No receipts yet.");
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("memory");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("");
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("");
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_signal(1);
        let _ = state::create_text_signal("");
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_text_signal("");
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_text_signal("");
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
        let _ = state::create_signal(0);
    });
}

fn with_session(body: impl FnOnce(&mut IntentSession)) {
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(IntentSession::open_local(
                AgentPermissions::interactive(),
                LocalIdentityStub::new(),
            ));
        }
        body(slot.as_mut().expect("shell intent session"));
    });
}

fn boot_session_once() {
    let already = SESSION_BOOTED.with(|booted| {
        let seen = booted.get();
        booted.set(true);
        seen
    });
    if already {
        return;
    }
    with_session(|session| {
        if session.authorize_bridge(&BridgeOp::ReadDom).is_ok() {
            capability_gate::grant(shell_tools::CAP_A11Y);
        }
        if session.authorize_bridge(&BridgeOp::Screenshot).is_ok() {
            capability_gate::grant(shell_tools::CAP_CAPTURE);
        }
        if std::env::var("W3COS_UNLOCK").ok().as_deref() == Some("1") {
            let passphrase = std::env::var("W3COS_UNLOCK_PASSPHRASE")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "w3cos-local".to_string());
            session.unlock_with_passphrase(&passphrase);
            lexicon::unlock(&passphrase);
        }
        sync_session(session, 0, "");
    });
}

fn ime_role_for_submit(code: u8) -> Option<&'static str> {
    match code {
        OP_PROPOSE => Some("intent"),
        OP_RUN_TERMINAL => Some("terminal"),
        OP_LAUNCHER_SEARCH => Some("launcher"),
        OP_RECEIPT_QUERY => Some("text"),
        OP_SIGN_PROPOSE => Some("sign"),
        _ => None,
    }
}

fn apply_ime_field() {
    for update in ime_bar::take_field_updates() {
        match update.role.as_str() {
            "intent" => state::set_text_signal(DRAFT_SIGNAL, update.text),
            "terminal" => state::set_text_signal(TERMINAL_SIGNAL, update.text),
            "launcher" => state::set_text_signal(LAUNCHER_QUERY_SIGNAL, update.text),
            "text" => state::set_text_signal(RECEIPT_QUERY_SIGNAL, update.text),
            "secure" => state::set_text_signal(LOCK_PASSPHRASE_SIGNAL, update.text),
            "sign" => state::set_text_signal(SIGN_MESSAGE_SIGNAL, update.text),
            _ => {}
        }
    }
}

fn on_shell_intent(code: u8, draft: &str) {
    if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
        eprintln!("[W3C OS][INTENT] code={code} draft={draft:?}");
    }
    ensure_shell_signals();
    if code == OP_IME_ACTION {
        let Some(utterance) = ime_bar::command_utterance() else {
            return;
        };
        let ctx = ShellContext {
            app: ShellApp::from_signal(state::get_signal(APP_SIGNAL)),
            night: state::get_signal(NIGHT_SIGNAL) != 0,
        };
        with_session(|session| apply_propose(session, &utterance, &ctx));
        return;
    }
    let mut draft_owned = draft.to_string();
    if let Some(role) = ime_role_for_submit(code)
        && let Some(text) = ime_bar::commit_active(role)
    {
        apply_ime_field();
        draft_owned = text;
    }
    let draft = draft_owned.as_str();
    if code == OP_TOGGLE_TASKS {
        let next = i64::from(state::get_signal(TASKS_SIGNAL) == 0);
        state::set_signal(TASKS_SIGNAL, next);
        return;
    }
    if handle_chrome_op(code, draft) {
        return;
    }
    let ctx = ShellContext {
        app: ShellApp::from_signal(state::get_signal(APP_SIGNAL)),
        night: state::get_signal(NIGHT_SIGNAL) != 0,
    };
    with_session(|session| match code {
        OP_PROPOSE => apply_propose(session, draft, &ctx),
        OP_CHIP_FILES => apply_propose(session, "open files", &ctx),
        OP_CHIP_NIGHT => apply_propose(session, "night mode", &ctx),
        OP_CHIP_APPS => apply_propose(session, "list apps", &ctx),
        OP_CHIP_CONTEXT => apply_propose(session, "where am i", &ctx),
        OP_CHIP_TERMINAL => apply_propose(session, "打开终端", &ctx),
        OP_CHIP_SETTINGS => apply_propose(session, "打开设置", &ctx),
        OP_CONFIRM => {
            let pending = state::get_signal_text(PENDING_LOCALE_SIGNAL);
            if !pending.is_empty() && state::get_signal(PHASE_SIGNAL) != 1 {
                apply_locale_confirm(session, &pending);
            } else {
                apply_confirm(session, &ctx);
            }
        }
        OP_CANCEL => {
            if state::get_signal(HOST_SIGNAL) != 0 {
                cancel_host_proposal(session);
                return;
            }
            if state::get_signal(TOOL_SIGNAL) != 0 {
                cancel_tool_proposal(session);
                return;
            }
            let pending = state::get_signal_text(PENDING_LOCALE_SIGNAL);
            if !pending.is_empty()
                && state::get_signal(PHASE_SIGNAL) != 1
                && state::get_signal(PHASE_SIGNAL) != 3
                && !w3cos_ai_bridge::agent_loop::has_pending()
            {
                state::set_text_signal(PENDING_LOCALE_SIGNAL, "");
                sync_session(session, state::get_signal(PHASE_SIGNAL), "");
            } else if state::get_signal(PHASE_SIGNAL) == 3
                || w3cos_ai_bridge::agent_loop::has_pending()
            {
                let update = agent_host::cancel();
                apply_host_update(session, update);
            } else {
                let closing_confirm = state::get_signal(PHASE_SIGNAL) == 1;
                session.cancel();
                sync_session(session, 0, "");
                if closing_confirm {
                    restore_after_dialog();
                }
            }
        }
        OP_REVOKE => {
            session.revoke_context();
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("toast-context-hidden"),
            );
        }
        OP_RESTORE => {
            session.restore_context();
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("toast-context-shared"),
            );
        }
        OP_LOCK => {
            let keep_card = state::get_signal(PHASE_SIGNAL) == 1;
            let summary = state::get_signal_text(SUMMARY_SIGNAL);
            let impact = state::get_signal_text(IMPACT_SIGNAL);
            let tier = state::get_signal_text(TIER_SIGNAL);
            let planning =
                state::get_signal(PHASE_SIGNAL) == 3 || w3cos_ai_bridge::agent_loop::has_pending();
            session.lock_identity();
            lexicon::lock();
            if planning {
                let update = agent_host::cancel();
                apply_host_update(session, update);
            }
            if session.proposal().is_some() {
                session.cancel();
            }
            if keep_card {
                state::set_text_signal(SUMMARY_SIGNAL, &summary);
                state::set_text_signal(IMPACT_SIGNAL, &impact);
                state::set_text_signal(TIER_SIGNAL, &tier);
                sync_session(session, 1, &locale::t("toast-identity-locked"));
            } else {
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("toast-identity-locked"),
                );
            }
        }
        OP_UNLOCK => {
            let passphrase = draft.trim();
            if session.unlock_with_passphrase(passphrase) {
                state::set_text_signal(LOCK_PASSPHRASE_SIGNAL, "");
                lexicon::unlock(passphrase);
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("toast-unlocked-stub"),
                );
            } else {
                state::set_text_signal(LOCK_PASSPHRASE_SIGNAL, passphrase);
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("toast-unlock-fail"),
                );
            }
        }
        OP_ENROLL => {
            let notice = if !session.identity_unlocked() {
                locale::t("toast-identity-locked")
            } else if enroll_device_passkey(session) {
                locale::t("toast-passkey-ok")
            } else {
                locale::t("toast-passkey-fail")
            };
            sync_session(session, state::get_signal(PHASE_SIGNAL), &notice);
        }
        OP_SHARED_APP => {
            shared_launch::spawn_detached(
                "app",
                &shared_launch::configured_path(
                    "W3COS_SHARED_APP",
                    "/usr/share/w3cos/apps/shared-hello/shared-hello.w3app",
                ),
            );
            state::set_text_signal(NOTICE_SIGNAL, locale::t("toast-shared-app"));
        }
        OP_SHARED_PAGE => {
            shared_launch::spawn_detached(
                "page",
                &shared_launch::configured_path(
                    "W3COS_SHARED_PAGE",
                    "/usr/share/w3cos/apps/shared-hello/page.html",
                ),
            );
            state::set_text_signal(NOTICE_SIGNAL, locale::t("toast-shared-page"));
        }
        OP_RUN_TERMINAL => match terminal_plan(&state::get_signal_text(TERMINAL_SIGNAL)) {
            TerminalPlan::Blocked(shown) => {
                let line = state::get_signal_text(TERMINAL_SIGNAL);
                refuse_terminal_without_grant(session, &line, &shown);
            }
            TerminalPlan::Allowlisted => open_process_permission(session),
            TerminalPlan::Sandbox => open_sandbox_permission(session),
        },
        OP_PERM_ONCE => {
            if state::get_signal(SANDBOX_SIGNAL) != 0 {
                accept_sandbox(session);
            } else if !session.identity_unlocked() {
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("perm-locked"),
                );
            } else {
                state::set_signal(PERM_SIGNAL, 0);
                run_terminal_from_button(session);
                restore_after_dialog();
            }
        }
        OP_PERM_DENY => {
            if state::get_signal(SANDBOX_SIGNAL) != 0 {
                if session.identity_unlocked() {
                    deny_sandbox(session);
                } else {
                    sync_session(
                        session,
                        state::get_signal(PHASE_SIGNAL),
                        &locale::t("sandbox-locked"),
                    );
                }
            } else if session.identity_unlocked() {
                deny_process_permission(session);
                restore_after_dialog();
            } else {
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("perm-locked"),
                );
            }
        }
        OP_SANDBOX_FS => {
            if state::get_signal(SANDBOX_SIGNAL) != 0 {
                toggle_sandbox_cap(SANDBOX_FS_SIGNAL);
            }
        }
        OP_SANDBOX_NET => {
            if state::get_signal(SANDBOX_SIGNAL) != 0 {
                toggle_sandbox_cap(SANDBOX_NET_SIGNAL);
            }
        }
        OP_SIGN_PROPOSE => open_sign_permission(session, draft),
        OP_SIGN_ONCE => accept_sign_permission(session),
        OP_SIGN_DENY => deny_sign_permission(session),
        OP_TX_SEND => accept_broadcast(session),
        OP_TX_HOLD => deny_broadcast(session),
        OP_CHAIN_ONCE => accept_chain_read(session),
        OP_CHAIN_DENY => deny_chain_read(session),
        OP_CHAIN_SAVE => save_chain_endpoint(session, draft),
        OP_L2_TOGGLE => toggle_l2(session),
        OP_INFER_BACKEND => toggle_infer_backend(session),
        OP_SHARED_PLUGIN => {
            shared_launch::spawn_detached(
                "plugin",
                &shared_launch::configured_path(
                    "W3COS_SHARED_PLUGIN",
                    "/usr/share/w3cos/apps/shared-counter/libshared_counter.so",
                ),
            );
            state::set_text_signal(NOTICE_SIGNAL, locale::t("toast-shared-plugin"));
        }
        _ => {}
    });
}

fn apply_propose(session: &mut IntentSession, utterance: &str, ctx: &ShellContext) {
    state::set_text_signal(DRAFT_SIGNAL, utterance);
    match session.propose(utterance, ctx) {
        ProposeResult::Confirm(proposal) => {
            let focus = if matches!(proposal.command, ShellCommand::ToggleNight) {
                "cancel"
            } else {
                "confirm"
            };
            state::set_text_signal(SUMMARY_SIGNAL, confirm_title(&proposal.command, ctx));
            state::set_text_signal(
                IMPACT_SIGNAL,
                confirm_impact(&proposal.command, session.context_shared()),
            );
            state::set_text_signal(TIER_SIGNAL, tier_name(proposal.tier));
            state::set_text_signal(CONFIRM_FOCUS_SIGNAL, focus);
            sync_session(session, 1, "");
            if session.identity_unlocked() {
                state::request_focus_label(&locale::t(focus));
            }
        }
        ProposeResult::Plan { utterance } => match agent_host::begin(&utterance) {
            Ok(update) => apply_host_update(session, update),
            Err(err) => {
                let receipt = session.record_external(
                    &utterance,
                    "l2",
                    w3cos_ai_bridge::intent::Decision::Unrecognized,
                    false,
                    &err,
                    IntentTier::L2,
                );
                state::set_text_signal(SUMMARY_SIGNAL, "");
                state::set_text_signal(IMPACT_SIGNAL, "");
                state::set_text_signal(TIER_SIGNAL, "L2");
                sync_session(session, 2, &receipt.outcome);
            }
        },
        ProposeResult::Unrecognized { receipt } => {
            state::set_text_signal(SUMMARY_SIGNAL, "");
            state::set_text_signal(IMPACT_SIGNAL, "");
            state::set_text_signal(TIER_SIGNAL, "");
            sync_session(session, 2, &receipt.outcome);
        }
    }
}

fn cancel_host_proposal(session: &mut IntentSession) {
    apply_host_mail(session);
    match sign_host::cancel(&locale::current(), now_ms()) {
        Ok(receipt) => {
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
        }
        Err(text) => {
            w3cos_ai_bridge::tool_host::fail_host(&text);
        }
    }
    state::set_signal(HOST_SIGNAL, 0);
    state::set_signal(PHASE_SIGNAL, 0);
    state::set_text_signal(SUMMARY_SIGNAL, "");
    state::set_text_signal(IMPACT_SIGNAL, "");
    sync_session(session, 0, &locale::t("tool-cancelled-notice"));
    restore_after_dialog();
}

fn cancel_tool_proposal(session: &mut IntentSession) {
    let summary = state::get_signal_text(SUMMARY_SIGNAL);
    let name = w3cos_ai_bridge::mcp::cancel().unwrap_or_else(|| "mcp".to_string());
    session.record_external(
        &summary,
        &name,
        Decision::Cancelled,
        false,
        &locale::t("tool-cancelled-notice"),
        IntentTier::L2,
    );
    state::set_signal(TOOL_SIGNAL, 0);
    state::set_text_signal(SUMMARY_SIGNAL, "");
    state::set_text_signal(IMPACT_SIGNAL, "");
    sync_session(session, 0, &locale::t("tool-cancelled-notice"));
    restore_after_dialog();
}

fn record_tool_outcome(session: &mut IntentSession, done: &w3cos_ai_bridge::mcp::ToolOutcome) {
    let decision = if (200..300).contains(&done.status) {
        Decision::Allowed
    } else {
        Decision::Denied
    };
    session.record_external(
        &done.summary,
        &done.name,
        decision,
        true,
        &done.note,
        IntentTier::L2,
    );
    let phase = state::get_signal(PHASE_SIGNAL);
    sync_session(session, phase, &done.note);
}

fn apply_confirm(session: &mut IntentSession, ctx: &ShellContext) {
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("locked-hint"),
        );
        return;
    }
    if state::get_signal(HOST_SIGNAL) != 0 {
        apply_host_mail(session);
        match sign_host::confirm(true, &locale::current(), now_ms()) {
            Ok(receipt) => {
                publish_host_receipt(&receipt);
                session.record_signed(receipt);
                state::set_signal(HOST_SIGNAL, 0);
                sync_session(session, 0, &locale::t("host-window-confirmed"));
            }
            Err(text) => {
                state::set_signal(HOST_SIGNAL, 0);
                w3cos_ai_bridge::tool_host::fail_host(&text);
                sync_session(session, 0, &text);
            }
        }
        restore_after_dialog();
        return;
    }
    if state::get_signal(TOOL_SIGNAL) != 0 {
        if w3cos_ai_bridge::mcp::arm() {
            state::set_signal(TOOL_SIGNAL, 0);
            sync_session(session, 0, &locale::t("tool-armed-notice"));
        } else {
            state::set_signal(TOOL_SIGNAL, 0);
            sync_session(session, 0, &locale::t("mcp-nothing"));
        }
        restore_after_dialog();
        return;
    }
    if w3cos_ai_bridge::agent_loop::has_pending() {
        confirm_agent_step(session);
        restore_after_dialog();
        return;
    }
    let Some(effect) = session.confirm(ctx, now_ms()) else {
        sync_session(session, 0, "");
        restore_after_dialog();
        return;
    };
    apply_effect(&effect);
    sync_session(session, 2, &effect.receipt.outcome);
    restore_after_dialog();
}

fn tier_name(tier: IntentTier) -> &'static str {
    match tier {
        IntentTier::L2 => "L2",
        IntentTier::L1 => "L1",
        IntentTier::L0 => "L0",
        IntentTier::Unspecified => "",
    }
}

fn confirm_title(command: &ShellCommand, ctx: &ShellContext) -> String {
    match command {
        ShellCommand::OpenApp(ShellApp::Files) => locale::t("chip-files"),
        ShellCommand::OpenApp(ShellApp::Terminal) => locale::t("chip-terminal"),
        ShellCommand::OpenApp(ShellApp::Settings) => locale::t("chip-settings"),
        ShellCommand::OpenApp(ShellApp::Browser) => locale::t("confirm-open-browser"),
        ShellCommand::OpenApp(ShellApp::Editor) => locale::t("confirm-open-editor"),
        ShellCommand::OpenApp(ShellApp::Agent) => locale::t("confirm-open-agent"),
        ShellCommand::OpenApp(ShellApp::Desktop) => locale::t("confirm-open-desktop"),
        ShellCommand::ToggleNight if ctx.night => locale::t("confirm-night-off"),
        ShellCommand::ToggleNight => locale::t("confirm-night-on"),
        ShellCommand::ListApps => locale::t("chip-apps"),
        ShellCommand::ShowContext => locale::t("confirm-context"),
        ShellCommand::Help => locale::t("confirm-help"),
    }
}

fn confirm_impact(command: &ShellCommand, shared: bool) -> String {
    match command {
        ShellCommand::OpenApp(ShellApp::Agent) => locale::t("impact-tasks"),
        ShellCommand::OpenApp(app) => {
            let name = app_label(app.signal_value());
            locale::t_args("impact-navigate", &[("name", &name)])
        }
        ShellCommand::ToggleNight => locale::t("impact-night"),
        ShellCommand::ListApps => locale::t("impact-list"),
        ShellCommand::ShowContext if shared => locale::t("impact-context"),
        ShellCommand::ShowContext => locale::t("impact-context-hidden"),
        ShellCommand::Help => locale::t("impact-help"),
    }
}

fn on_agent_poll() {
    ensure_shell_signals();
    with_session(apply_host_mail);
    if let Some(done) = w3cos_ai_bridge::mcp::take_done() {
        with_session(|session| record_tool_outcome(session, &done));
    }
    let (can_read, can_shot) = session_read_flags();
    let mut view = current_shell_view();
    let Some(update) = agent_host::poll(&mut view, &|name| match name {
        shell_tools::CAP_A11Y => can_read,
        shell_tools::CAP_CAPTURE => can_shot,
        _ => false,
    }) else {
        return;
    };
    with_session(|session| apply_host_update(session, update));
}

fn confirm_agent_step(session: &mut IntentSession) {
    let name = w3cos_ai_bridge::agent_loop::pending_capability();
    if name == shell_tools::CAP_A11Y || name == shell_tools::CAP_CAPTURE {
        let mut view = current_shell_view();
        let update = agent_host::confirm(&mut view, &|capability| {
            capability_gate::is_granted(capability)
        });
        apply_host_update(session, update);
        return;
    }
    let tool_name = if name == shell_tools::CAP_PROCESS {
        "click"
    } else {
        "act"
    };
    if let Err(reason) = session.gate_mutation(now_ms()) {
        apply_host_update(session, agent_host::reject(reason));
        return;
    }
    let op = BridgeOp::Mutate {
        action: ActionType::Click,
        selector: format!("shell:l2:{tool_name}"),
    };
    if let Err(reason) = session.authorize_bridge(&op) {
        apply_host_update(session, agent_host::reject(&reason));
        return;
    }
    capability_gate::grant(shell_tools::CAP_ACT);
    if name == shell_tools::CAP_PROCESS {
        capability_gate::grant(shell_tools::CAP_PROCESS);
    }
    let mut view = current_shell_view();
    let update = agent_host::confirm(&mut view, &|capability| {
        capability_gate::is_granted(capability)
    });
    capability_gate::deny(shell_tools::CAP_ACT);
    capability_gate::deny(shell_tools::CAP_PROCESS);
    apply_host_update(session, update);
}

fn session_read_flags() -> (bool, bool) {
    let mut can_read = false;
    let mut can_shot = false;
    with_session(|session| {
        can_read = session.authorize_bridge(&BridgeOp::ReadDom).is_ok()
            && capability_gate::is_granted(shell_tools::CAP_A11Y);
        can_shot = session.authorize_bridge(&BridgeOp::Screenshot).is_ok()
            && capability_gate::is_granted(shell_tools::CAP_CAPTURE);
    });
    (can_read, can_shot)
}

fn current_shell_view() -> ShellView {
    ShellView {
        app: ShellApp::from_signal(state::get_signal(APP_SIGNAL)),
        night: state::get_signal(NIGHT_SIGNAL) != 0,
        terminal_line: state::get_signal_text(TERMINAL_SIGNAL),
        terminal_output: state::get_signal_text(TERM_OUT_SIGNAL),
    }
}

fn apply_host_update(session: &mut IntentSession, update: agent_host::HostUpdate) {
    for effect in &update.effects {
        match effect {
            shell_tools::ShellEffect::Navigate(app) => {
                focus_app(app.signal_value());
            }
            shell_tools::ShellEffect::SetNight(night) => {
                state::set_signal(NIGHT_SIGNAL, i64::from(*night));
            }
            shell_tools::ShellEffect::SetTerminalLine(line) => {
                state::set_text_signal(TERMINAL_SIGNAL, line);
            }
            shell_tools::ShellEffect::SetTerminalOutput(output) => {
                state::set_text_signal(TERM_OUT_SIGNAL, output);
            }
        }
    }
    for receipt in &update.receipts {
        session.record_external(
            &update.utterance,
            &receipt.command,
            receipt.decision,
            receipt.confirmed,
            &receipt.outcome,
            receipt.tier,
        );
    }
    if update.open_tasks {
        state::set_signal(TASKS_SIGNAL, 1);
    }
    state::set_text_signal(SUMMARY_SIGNAL, update.summary);
    state::set_text_signal(IMPACT_SIGNAL, update.impact);
    state::set_text_signal(TIER_SIGNAL, update.tier);
    state::set_text_signal(STEPS_SIGNAL, update.steps);
    if update.phase == 1 {
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "cancel");
        if session.identity_unlocked() {
            state::request_focus_label(&locale::t("cancel"));
        }
    }
    sync_session(session, update.phase, &update.notice);
    if let Ok(path) = std::env::var("W3COS_AGENT_TRACE") {
        let line = format!(
            "phase={} {}\n",
            update.phase,
            update.notice.replace('\n', " ")
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(line.as_bytes())
            });
    }
}

fn terminal_has_metacharacters(line: &str) -> bool {
    line.chars().any(|ch| {
        matches!(
            ch,
            ';' | '|' | '&' | '`' | '$' | '\n' | '>' | '<' | '\\' | '(' | ')' | '\'' | '"'
        )
    })
}

enum TerminalPlan {
    Blocked(String),
    Allowlisted,
    Sandbox,
}

/// `echo`, `pwd`, and `help` still ask for the one-shot process grant.
/// Anything else opens the sandbox sheet. A shell metacharacter never runs.
fn terminal_plan(line: &str) -> TerminalPlan {
    let line = line.trim();
    if line.is_empty() {
        return TerminalPlan::Blocked(locale::t("terminal-empty"));
    }
    if terminal_has_metacharacters(line) {
        return TerminalPlan::Blocked(locale::t("terminal-meta"));
    }
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or("");
    let has_args = parts.next().is_some();
    match command {
        "echo" => TerminalPlan::Allowlisted,
        "pwd" | "help" if !has_args => TerminalPlan::Allowlisted,
        _ => TerminalPlan::Sandbox,
    }
}

fn terminal_refusal(line: &str, err: &str) -> String {
    let err = err.to_ascii_lowercase();
    if err.contains("empty") {
        return locale::t("terminal-empty");
    }
    if err.contains("metacharacter") {
        return locale::t("terminal-meta");
    }
    let command = line.split_whitespace().next().unwrap_or("");
    if command.is_empty() {
        locale::t("terminal-empty")
    } else if err.contains("allowlist") || err.contains("refused") {
        locale::t_args("terminal-refused", &[("command", command)])
    } else {
        locale::t("terminal-failed")
    }
}

fn refuse_terminal_without_grant(session: &mut IntentSession, line: &str, shown: &str) {
    state::set_signal(PERM_SIGNAL, 0);
    if line.trim().is_empty() {
        state::set_text_signal(TERM_OUT_SIGNAL, shown);
        return;
    }
    if !session.identity_unlocked() {
        state::set_text_signal(TERM_OUT_SIGNAL, &locale::t("perm-locked"));
        return;
    }
    state::set_text_signal(TERM_OUT_SIGNAL, shown);
    let err = if terminal_has_metacharacters(line) {
        "refused: the command has shell metacharacters"
    } else {
        "refused: not allowlisted (echo, pwd, help)"
    };
    session.record_external(
        line,
        "l2:run",
        w3cos_ai_bridge::intent::Decision::Denied,
        true,
        &format!("Denied: {err}. Nothing was changed."),
        IntentTier::L0,
    );
    sync_session(session, 2, &locale::t("perm-denied-notice"));
}

fn run_terminal_from_button(session: &mut IntentSession) {
    let line = state::get_signal_text(TERMINAL_SIGNAL);
    if let Err(reason) = session.gate_mutation(now_ms()) {
        sync_session(session, state::get_signal(PHASE_SIGNAL), reason);
        return;
    }
    let op = BridgeOp::Mutate {
        action: ActionType::Click,
        selector: "shell:l2:run".into(),
    };
    if let Err(reason) = session.authorize_bridge(&op) {
        sync_session(session, state::get_signal(PHASE_SIGNAL), &reason);
        return;
    }
    capability_gate::grant(shell_tools::CAP_PROCESS);
    let granted = capability_gate::is_granted(shell_tools::CAP_PROCESS);
    let (shown, outcome, denied) = if !granted {
        (
            locale::t("terminal-failed"),
            "Denied: w3cos-process is not granted. Nothing was changed.".to_string(),
            true,
        )
    } else {
        match shell_tools::run_allowlisted(&line) {
            Ok(output) => {
                let shown = if line.trim() == "help" {
                    locale::t("terminal-help")
                } else {
                    output.clone()
                };
                (shown, output, false)
            }
            Err(err) => (
                terminal_refusal(&line, &err),
                format!("Denied: {err}. Nothing was changed."),
                true,
            ),
        }
    };
    capability_gate::deny(shell_tools::CAP_PROCESS);
    state::set_text_signal(TERM_OUT_SIGNAL, &shown);
    let decision = if denied {
        w3cos_ai_bridge::intent::Decision::Denied
    } else {
        w3cos_ai_bridge::intent::Decision::Allowed
    };
    session.record_external(&line, "l2:run", decision, true, &outcome, IntentTier::L2);
    if denied {
        sync_session(session, 2, &locale::t("perm-denied-notice"));
    } else {
        sync_session(session, 2, &outcome);
    }
}

fn show_settings_pane(tab: i64) {
    state::set_signal(SETTINGS_TAB_SIGNAL, tab);
    state::set_signal(SETTINGS_MENU_SIGNAL, 0);
}

fn stage_locale(tag: &str) {
    if locale::current() == tag || state::get_signal(PHASE_SIGNAL) == 1 {
        state::set_text_signal(PENDING_LOCALE_SIGNAL, "");
        return;
    }
    state::set_text_signal(PENDING_LOCALE_SIGNAL, tag);
    state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "cancel");
    if state::get_signal(UNLOCKED_SIGNAL) != 0 {
        state::request_focus_label(&locale::t("cancel"));
    }
    state::mark_dirty();
}

fn locale_self_name(tag: &str) -> String {
    if tag == "en" {
        locale::t("locale-name-en")
    } else {
        locale::t("locale-name-zh")
    }
}

fn apply_locale_confirm(session: &mut IntentSession, tag: &str) {
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("lock-first"),
        );
        return;
    }
    let name = locale_self_name(tag);
    let utterance = locale::t_args("locale-confirm-title", &[("name", &name)]);
    locale::set_locale(tag);
    state::set_text_signal(PENDING_LOCALE_SIGNAL, "");
    let applied = locale_self_name(tag);
    let outcome = format!("Interface language is now {tag}.");
    session.record_external(
        &utterance,
        "set-locale",
        w3cos_ai_bridge::intent::Decision::Allowed,
        true,
        &outcome,
        IntentTier::L0,
    );
    sync_session(
        session,
        state::get_signal(PHASE_SIGNAL),
        &locale::t_args("locale-applied", &[("name", &applied)]),
    );
}

fn apply_effect(effect: &ExecuteResult) {
    if let Some(app) = effect.navigate {
        // The agent is not a window. Confirming it opens the tray task center.
        focus_app(app.signal_value());
    }
    if let Some(night) = effect.night {
        state::set_signal(NIGHT_SIGNAL, i64::from(night));
    }
}

fn sync_session(session: &IntentSession, phase: i64, notice: &str) {
    state::set_signal(PHASE_SIGNAL, phase);
    state::set_signal(CONTEXT_SIGNAL, i64::from(session.context_shared()));
    state::set_signal(UNLOCKED_SIGNAL, i64::from(session.identity_unlocked()));
    state::set_text_signal(NOTICE_SIGNAL, notice);
    if !notice.is_empty() {
        state::set_text_signal(TOAST_SIGNAL, notice);
    }
    state::set_text_signal(RECEIPT_SIGNAL, session.ledger_log_tail(48));
    let added = absorb_new_receipts();
    let localized = notice
        .chars()
        .any(|ch| ('\u{2E80}'..='\u{9FFF}').contains(&ch));
    if !notice.is_empty() && localized && (added == 0 || notice != locale::t("perm-denied-notice"))
    {
        state::set_text_signal(TOAST_SIGNAL, notice);
        state::set_text_signal(NOTICE_SIGNAL, notice);
        remember_notice(notice);
    } else if added > 0 {
        if let Some(title) = latest_notice_title() {
            state::set_text_signal(TOAST_SIGNAL, &title);
            state::set_text_signal(NOTICE_SIGNAL, &title);
        }
    } else if !notice.is_empty() {
        state::set_text_signal(TOAST_SIGNAL, notice);
        state::set_text_signal(NOTICE_SIGNAL, notice);
        remember_notice(notice);
    }
    state::set_text_signal(STORAGE_SIGNAL, session.receipt_storage());
    state::set_text_signal(SUBJECT_SIGNAL, session.actor());
    let enroll = match session.enrollment_state() {
        w3cos_ai_bridge::identity::EnrollmentState::AssertionVerified => "verified",
        w3cos_ai_bridge::identity::EnrollmentState::KeyMaterialStored => "stored",
        w3cos_ai_bridge::identity::EnrollmentState::AwaitingPasskey => "awaiting",
        w3cos_ai_bridge::identity::EnrollmentState::Stub => "stub",
    };
    state::set_text_signal(ENROLL_SIGNAL, enroll);
}

fn enroll_device_passkey(session: &mut IntentSession) -> bool {
    let Some(challenge) = session.begin_passkey_enrollment() else {
        return false;
    };
    let device = w3cos_p2p::DeviceIdentity::from_seed(handoff::device_seed());
    let Some(assertion) = w3cos_ai_bridge::identity::device_assertion(
        &challenge,
        &device.ed25519_public,
        |message| device.sign(message).ok(),
    ) else {
        return false;
    };
    session.accept_webauthn_assertion(&assertion, &device.ed25519_public)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn invoke(code: u8) -> EventAction {
    EventAction::Invoke {
        code,
        input_placeholder: String::new(),
    }
}

fn app_key(id: i64) -> &'static str {
    match id {
        1 => "app-files",
        2 => "app-terminal",
        3 => "app-settings",
        4 => "app-agent",
        5 => "app-browser",
        6 => "app-editor",
        _ => "app-desktop",
    }
}

fn app_label(id: i64) -> String {
    locale::t(app_key(id))
}

fn app_accent(id: i64) -> &'static str {
    match id {
        1 => "#3498db",
        2 => "#1c8f62",
        3 => "#f0d7b4",
        4 => "#e07a5f",
        5 => "#8ec8ff",
        _ => "#d5dee8",
    }
}

fn toast_named(key: &str, id: i64) -> String {
    let name = app_label(id);
    locale::t_args(key, &[("name", &name)])
}

fn app_bit(id: i64) -> i64 {
    if (1..=6).contains(&id) {
        1_i64 << id
    } else {
        0
    }
}

fn focus_code(id: i64) -> u8 {
    match id {
        0 => OP_FOCUS_DESKTOP,
        1 => OP_FOCUS_FILES,
        2 => OP_FOCUS_TERMINAL,
        3 => OP_FOCUS_SETTINGS,
        4 => OP_FOCUS_AGENT,
        5 => OP_FOCUS_BROWSER,
        _ => OP_FOCUS_EDITOR,
    }
}

fn push_toast(text: &str) {
    state::set_text_signal(TOAST_SIGNAL, text);
    remember_notice(text);
}

fn remember_notice(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    post_notice(text, "", "");
}

fn latest_notice_title() -> Option<String> {
    NOTICES.with(|slot| slot.borrow().last().map(|notice| notice.title.clone()))
}

fn post_notice(title: &str, detail: &str, draft: &str) {
    let title = title.trim();
    if title.is_empty() {
        return;
    }
    let notice = ShellNotice {
        title: title.to_string(),
        detail: detail.trim().to_string(),
        draft: draft.to_string(),
    };
    let added = NOTICES.with(|slot| {
        let mut notices = slot.borrow_mut();
        if notices.last().is_some_and(|last| last == &notice) {
            return false;
        }
        if notices.iter().any(|item| item == &notice) {
            return false;
        }
        notices.push(notice);
        if notices.len() > 12 {
            notices.remove(0);
        }
        true
    });
    if added && state::get_signal(NOTIFY_SIGNAL) == 0 {
        state::set_signal(
            NOTIFY_UNREAD_SIGNAL,
            state::get_signal(NOTIFY_UNREAD_SIGNAL) + 1,
        );
    }
}

fn receipt_notice_sub(parts: &ReceiptParts) -> String {
    let when = notice_when(&parts.recorded_at);
    let tier = layer_label(if parts.tier.is_empty() {
        "L0"
    } else {
        parts.tier.as_str()
    });
    locale::t_args(
        "notify-receipt-sub",
        &[("id", &parts.id), ("tier", &tier), ("when", &when)],
    )
}

fn notice_when(recorded_at: &str) -> String {
    let Ok(millis) = recorded_at.parse::<i64>() else {
        return locale::t("notify-just-now");
    };
    let now = now_ms() as i64;
    if (now - millis).abs() <= 120_000 {
        return locale::t("notify-just-now");
    }
    let clock = format_receipt_clock(recorded_at);
    if clock.is_empty() {
        locale::t("notify-just-now")
    } else {
        clock
    }
}

fn app_name_for_slug(slug: &str) -> String {
    match slug {
        "files" => locale::t("app-files"),
        "terminal" => locale::t("app-terminal"),
        "settings" => locale::t("app-settings"),
        "browser" => locale::t("app-browser"),
        "editor" => locale::t("app-editor"),
        "desktop" => locale::t("app-desktop"),
        "agent" => locale::t("tasks-title"),
        other => other.to_string(),
    }
}

fn notice_from_receipt(line: &str) -> Option<ShellNotice> {
    let parts = parse_receipt(line);
    if !parts.id.starts_with('#') || parts.id == "#" {
        return None;
    }
    let draft = format!("receipt:{}", parts.id);
    let (title, detail) = match parts.decision.as_str() {
        "allowed" => {
            let title = if let Some(slug) = parts.command.strip_prefix("open:") {
                locale::t_args("notify-opened", &[("name", &app_name_for_slug(slug))])
            } else if parts.command == "night" {
                if parts.detail.to_ascii_lowercase().contains("off") {
                    locale::t("notify-night-off")
                } else {
                    locale::t("notify-night-on")
                }
            } else if parts.command == "set-locale" {
                locale::t("notify-locale")
            } else if parts.detail.chars().any(|ch| !ch.is_ascii()) {
                parts.detail.clone()
            } else {
                locale::t("notify-recorded")
            };
            (title, receipt_notice_sub(&parts))
        }
        "denied" => {
            let command = if parts.command == "l2:run" {
                parts
                    .utterance
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string()
            } else {
                parts.command.clone()
            };
            let title = if command.is_empty() {
                locale::t("notify-denied-plain")
            } else {
                locale::t_args("notify-denied", &[("command", &command)])
            };
            let lowered = parts.detail.to_ascii_lowercase();
            let detail = if lowered.contains("locked") || parts.detail.contains("锁") {
                locale::t("notify-denied-locked")
            } else if lowered.contains("refused") || lowered.contains("denied") {
                locale::t("notify-denied-refused")
            } else {
                locale::t("notify-denied-generic")
            };
            (title, detail)
        }
        "cancelled" => (locale::t("notify-cancelled"), receipt_notice_sub(&parts)),
        "unrecognized" => (locale::t("notify-unrecognized"), receipt_notice_sub(&parts)),
        _ => (locale::t("notify-recorded"), receipt_notice_sub(&parts)),
    };
    Some(ShellNotice {
        title,
        detail,
        draft,
    })
}

/// Receipts already on the ledger at boot are history, not a fresh feed.
fn absorb_new_receipts() -> usize {
    let first = !RECEIPT_NOTICES_BASELINED.with(|flag| flag.replace(true));
    let mut added = 0;
    for line in receipt_entries() {
        let Some(notice) = notice_from_receipt(&line) else {
            continue;
        };
        let id = notice
            .draft
            .strip_prefix("receipt:")
            .unwrap_or("")
            .to_string();
        let seen = SEEN_RECEIPTS.with(|slot| {
            let mut seen = slot.borrow_mut();
            !seen.insert(id)
        });
        if seen || first {
            continue;
        }
        let before = NOTICES.with(|slot| slot.borrow().len());
        post_notice(&notice.title, &notice.detail, &notice.draft);
        if NOTICES.with(|slot| slot.borrow().len()) > before {
            added += 1;
        }
    }
    added
}

fn observe_live_notices() {
    if let Some(peer) = handoff::pending_peer() {
        post_notice(
            &locale::t_args("notify-peer", &[("name", &peer.name)]),
            &locale::t("notify-peer-detail"),
            "devices",
        );
    }
    if let Some((name, _, _)) = handoff::incoming() {
        post_notice(
            &locale::t_args("handoff-incoming", &[("name", &name)]),
            "",
            "incoming",
        );
    }
    let id = handoff::notice();
    if matches!(id.as_str(), "handoff-no-answer" | "handoff-sending") {
        post_notice(&handoff_notice_text(&id), "", "");
    }
}

fn open_notice_target(draft: &str) {
    state::set_signal(NOTIFY_SIGNAL, 0);
    if let Some(id) = draft.strip_prefix("receipt:") {
        state::set_signal(RECEIPT_FILTER_SIGNAL, 0);
        state::set_text_signal(RECEIPT_QUERY_SIGNAL, "");
        let entries = visible_receipts();
        let page = entries
            .iter()
            .position(|line| parse_receipt(line).id == id)
            .map(|index| (index / RECEIPT_PAGE_SIZE) as i64)
            .unwrap_or(0);
        state::set_signal(RECEIPT_PAGE_SIGNAL, page);
        NOTICE_FOCUS.with(|slot| *slot.borrow_mut() = id.to_string());
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 1);
    } else if draft == "devices" {
        focus_app(3);
        show_settings_pane(4);
    } else if draft == "incoming" {
        handoff::reveal_incoming();
    }
}

fn open_process_permission(session: &mut IntentSession) {
    clear_sandbox_sheet();
    state::set_signal(PERM_SIGNAL, 1);
    state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
    if session.identity_unlocked() {
        state::request_focus_label(&locale::t("perm-deny"));
    } else {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("perm-locked"),
        );
    }
}

fn sign_message_from(draft: &str) -> String {
    let typed = draft.trim();
    if !typed.is_empty() {
        return typed.to_string();
    }
    state::get_signal_text(SIGN_MESSAGE_SIGNAL)
        .trim()
        .to_string()
}

fn open_sign_permission(session: &mut IntentSession, draft: &str) {
    let message = sign_message_from(draft);
    if message.is_empty() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("host-missing-message"),
        );
        return;
    }
    state::set_text_signal(SIGN_MESSAGE_SIGNAL, &message);
    let unlocked = session.identity_unlocked();
    match sign_host::propose_wallet_sign(
        &message,
        unlocked,
        &locale::current(),
        now_ms(),
        session.signed_high_water().saturating_add(1),
    ) {
        Ok(proposal) => {
            state::set_text_signal(SIGN_SUMMARY_SIGNAL, &proposal.summary);
            state::set_text_signal(SIGN_IMPACT_SIGNAL, &proposal.impact);
            state::set_signal(SIGN_SIGNAL, 1);
            state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
            if unlocked {
                state::request_focus_label(&locale::t("sign-deny"));
            }
        }
        Err(text) => {
            sync_session(session, state::get_signal(PHASE_SIGNAL), &text);
        }
    }
}

fn accept_sign_permission(session: &mut IntentSession) {
    if state::get_signal(SIGN_SIGNAL) == 0 {
        return;
    }
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("sign-locked"),
        );
        return;
    }
    apply_host_mail(session);
    match sign_host::confirm(true, &locale::current(), now_ms()) {
        Ok(receipt) => {
            let offer_broadcast =
                receipt.command == "wallet_sign_tx" && receipt.decision == Decision::Allowed;
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
            state::set_signal(SIGN_SIGNAL, 0);
            if offer_broadcast {
                state::set_signal(BROADCAST_SIGNAL, 1);
                state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
                state::request_focus_label(&locale::t("broadcast-deny"));
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("sign-allowed-notice"),
                );
            } else {
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("sign-allowed-notice"),
                );
                restore_after_dialog();
            }
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn sample_tx_json() -> String {
    r#"{"chain_id":1,"nonce":7,"gas_price":"1000000000","gas_limit":21000,"to":"0x1111111111111111111111111111111111111111","value":"1","data":"0x"}"#
        .to_string()
}

fn open_tx_permission(session: &mut IntentSession) {
    let raw = sample_tx_json();
    let unlocked = session.identity_unlocked();
    match sign_host::propose_tool(
        "wallet_sign_tx",
        &raw,
        unlocked,
        &locale::current(),
        now_ms(),
        session.signed_high_water().saturating_add(1),
    ) {
        Ok(proposal) => {
            let shown = w3cos_ai_bridge::tx::preview_from_json(&locale::current(), &raw)
                .unwrap_or_default();
            state::set_text_signal(SIGN_MESSAGE_SIGNAL, &shown);
            state::set_text_signal(SIGN_SUMMARY_SIGNAL, &proposal.summary);
            state::set_text_signal(SIGN_IMPACT_SIGNAL, &proposal.impact);
            state::set_signal(SIGN_SIGNAL, 1);
            state::set_signal(BROADCAST_SIGNAL, 0);
            state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
            if unlocked {
                state::request_focus_label(&locale::t("sign-deny"));
            }
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn accept_broadcast(session: &mut IntentSession) {
    if state::get_signal(BROADCAST_SIGNAL) == 0 {
        return;
    }
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("broadcast-locked"),
        );
        return;
    }
    sign_host::set_chain_endpoint(&state::get_signal_text(CHAIN_RPC_SIGNAL));
    match sign_host::broadcast(true, &locale::current(), now_ms()) {
        Ok(receipt) => {
            let sent = receipt.decision == Decision::Allowed;
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
            state::set_signal(BROADCAST_SIGNAL, 0);
            let key = if sent {
                "broadcast-sent-notice"
            } else {
                "broadcast-held-notice"
            };
            sync_session(session, state::get_signal(PHASE_SIGNAL), &locale::t(key));
            restore_after_dialog();
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn deny_broadcast(session: &mut IntentSession) {
    if state::get_signal(BROADCAST_SIGNAL) == 0 {
        return;
    }
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("broadcast-locked"),
        );
        return;
    }
    match sign_host::decline_broadcast(&locale::current(), now_ms()) {
        Ok(receipt) => {
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
            state::set_signal(BROADCAST_SIGNAL, 0);
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("broadcast-held-notice"),
            );
            restore_after_dialog();
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn deny_sign_permission(session: &mut IntentSession) {
    if state::get_signal(SIGN_SIGNAL) == 0 {
        return;
    }
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("sign-locked"),
        );
        return;
    }
    apply_host_mail(session);
    match sign_host::cancel(&locale::current(), now_ms()) {
        Ok(receipt) => {
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
            state::set_signal(SIGN_SIGNAL, 0);
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("sign-denied-notice"),
            );
            restore_after_dialog();
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn save_chain_endpoint(session: &mut IntentSession, draft: &str) {
    let raw = draft.trim();
    if raw.is_empty() {
        state::set_text_signal(CHAIN_RPC_SIGNAL, "");
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("chain-rpc-cleared"),
        );
        return;
    }
    if w3cos_ai_bridge::chain::parse_endpoint(raw).is_none() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("chain-rpc-bad"),
        );
        return;
    }
    state::set_text_signal(CHAIN_RPC_SIGNAL, raw);
    sync_session(
        session,
        state::get_signal(PHASE_SIGNAL),
        &locale::t("chain-rpc-saved"),
    );
}

fn accept_chain_read(session: &mut IntentSession) {
    if state::get_signal(CHAIN_SIGNAL) == 0 {
        return;
    }
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("perm-locked"),
        );
        return;
    }
    sign_host::set_chain_endpoint(&state::get_signal_text(CHAIN_RPC_SIGNAL));
    apply_host_mail(session);
    match sign_host::confirm(true, &locale::current(), now_ms()) {
        Ok(receipt) => {
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
            state::set_signal(CHAIN_SIGNAL, 0);
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("chain-read-notice"),
            );
            restore_after_dialog();
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn deny_chain_read(session: &mut IntentSession) {
    if state::get_signal(CHAIN_SIGNAL) == 0 {
        return;
    }
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("perm-locked"),
        );
        return;
    }
    apply_host_mail(session);
    match sign_host::cancel(&locale::current(), now_ms()) {
        Ok(receipt) => {
            publish_host_receipt(&receipt);
            session.record_signed(receipt);
            state::set_signal(CHAIN_SIGNAL, 0);
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("chain-denied-notice"),
            );
            restore_after_dialog();
        }
        Err(text) => sync_session(session, state::get_signal(PHASE_SIGNAL), &text),
    }
}

fn clear_sandbox_sheet() {
    state::set_signal(SANDBOX_SIGNAL, 0);
    state::set_signal(SANDBOX_FS_SIGNAL, 0);
    state::set_signal(SANDBOX_NET_SIGNAL, 0);
}

fn toggle_sandbox_cap(signal: usize) {
    let next = i64::from(state::get_signal(signal) == 0);
    state::set_signal(signal, next);
}

fn open_sandbox_permission(session: &mut IntentSession) {
    state::set_signal(PERM_SIGNAL, 0);
    state::set_signal(SANDBOX_FS_SIGNAL, 0);
    state::set_signal(SANDBOX_NET_SIGNAL, 0);
    state::set_signal(SANDBOX_SIGNAL, 1);
    state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "sandbox-deny");
    if session.identity_unlocked() {
        state::request_focus_label(&locale::t("sandbox-deny"));
    } else {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("sandbox-locked"),
        );
    }
}

fn sandbox_command_label() -> String {
    let line = state::get_signal_text(TERMINAL_SIGNAL);
    let token = line.split_whitespace().next().unwrap_or("");
    let name = token
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(token);
    truncate_middle(name, 24)
}

fn split_sandbox_line(line: &str) -> Option<(String, Vec<String>)> {
    let mut parts = line.split_whitespace().map(str::to_string);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

fn accept_sandbox(session: &mut IntentSession) {
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("sandbox-locked"),
        );
        return;
    }
    let caps = w3cos_ai_bridge::sandbox::SandboxCaps {
        filesystem: state::get_signal(SANDBOX_FS_SIGNAL) != 0,
        network: state::get_signal(SANDBOX_NET_SIGNAL) != 0,
    };
    clear_sandbox_sheet();
    let line = state::get_signal_text(TERMINAL_SIGNAL);
    let Some((program, args)) = split_sandbox_line(&line) else {
        state::set_text_signal(TERM_OUT_SIGNAL, &locale::t("sandbox-missing"));
        record_sandbox(session, &line, false, "Denied: the program was not found.");
        restore_after_dialog();
        return;
    };
    match w3cos_ai_bridge::sandbox::run(&program, &args, caps) {
        Ok(output) => {
            let shown = if output.is_empty() {
                locale::t("sandbox-empty")
            } else {
                output
            };
            state::set_text_signal(TERM_OUT_SIGNAL, &shown);
            record_sandbox(session, &line, true, "Allowed: the sandbox ran once.");
        }
        Err(code) => {
            let (shown, outcome) = match code.as_str() {
                "missing" => (
                    locale::t("sandbox-missing"),
                    "Denied: the program was not found.",
                ),
                "no-file" => (
                    locale::t("sandbox-no-disk"),
                    "Denied: the sandbox has no files.",
                ),
                "timeout" => (
                    locale::t("sandbox-timeout"),
                    "Denied: the sandbox timed out.",
                ),
                _ => (
                    locale::t("sandbox-failed"),
                    "Denied: the sandbox did not finish.",
                ),
            };
            state::set_text_signal(TERM_OUT_SIGNAL, &shown);
            record_sandbox(session, &line, false, outcome);
        }
    }
    restore_after_dialog();
}

fn deny_sandbox(session: &mut IntentSession) {
    clear_sandbox_sheet();
    let line = state::get_signal_text(TERMINAL_SIGNAL);
    state::set_text_signal(TERM_OUT_SIGNAL, &locale::t("sandbox-denied-notice"));
    record_sandbox(
        session,
        &line,
        false,
        "Denied: the sandbox run was refused. Nothing was changed.",
    );
    restore_after_dialog();
}

fn record_sandbox(session: &mut IntentSession, line: &str, allowed: bool, outcome: &str) {
    let decision = if allowed {
        w3cos_ai_bridge::intent::Decision::Allowed
    } else {
        w3cos_ai_bridge::intent::Decision::Denied
    };
    session.record_external(
        line,
        "l2:run",
        decision,
        true,
        outcome,
        if allowed {
            IntentTier::L2
        } else {
            IntentTier::L0
        },
    );
    let notice = if allowed {
        locale::t("sandbox-ran")
    } else {
        locale::t("sandbox-denied-notice")
    };
    sync_session(session, 2, &notice);
}

fn deny_process_permission(session: &mut IntentSession) {
    state::set_signal(PERM_SIGNAL, 0);
    let line = state::get_signal_text(TERMINAL_SIGNAL);
    session.record_external(
        &line,
        "l2:run",
        w3cos_ai_bridge::intent::Decision::Denied,
        true,
        "Denied: the process grant was refused. Nothing was changed.",
        IntentTier::L0,
    );
    sync_session(session, 2, &locale::t("perm-denied-notice"));
}

fn scrub_agent_window() {
    let bit = app_bit(4);
    state::set_signal(OPEN_MASK_SIGNAL, state::get_signal(OPEN_MASK_SIGNAL) & !bit);
    state::set_signal(
        MINIMIZED_MASK_SIGNAL,
        state::get_signal(MINIMIZED_MASK_SIGNAL) & !bit,
    );
    if state::get_signal(APP_SIGNAL) == 4 {
        state::set_signal(APP_SIGNAL, 0);
    }
}

/// Planning, receipts, and steps live in the tray task center, not an app window.
fn open_task_center() {
    ensure_shell_signals();
    scrub_agent_window();
    state::set_signal(TASKS_SIGNAL, 1);
    state::set_signal(LAUNCHER_SIGNAL, 0);
}

fn focus_app(id: i64) {
    ensure_shell_signals();
    if id == 4 {
        open_task_center();
        return;
    }
    let id = id.clamp(0, 6);
    state::set_signal(APP_SIGNAL, id);
    state::set_signal(LAUNCHER_SIGNAL, 0);
    let bit = app_bit(id);
    if bit == 0 {
        return;
    }
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    state::set_signal(OPEN_MASK_SIGNAL, open | bit);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    state::set_signal(MINIMIZED_MASK_SIGNAL, minimized & !bit);
    windows::raise(id);
    remember_windows();
}

fn window_target(draft: &str) -> i64 {
    draft
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|id| (1..=6).contains(id))
        .unwrap_or_else(|| state::get_signal(APP_SIGNAL))
}

fn minimize_window(id: i64) {
    let bit = app_bit(id);
    if bit == 0 {
        push_toast(&locale::t("toast-desktop-stays"));
        return;
    }
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    state::set_signal(OPEN_MASK_SIGNAL, open | bit);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    state::set_signal(MINIMIZED_MASK_SIGNAL, minimized | bit);
    if state::get_signal(APP_SIGNAL) == id {
        state::set_signal(APP_SIGNAL, 0);
    }
    push_toast(&toast_named("toast-minimized", id));
    remember_windows();
}

fn close_window(id: i64) {
    let bit = app_bit(id);
    if bit == 0 {
        push_toast(&locale::t("toast-no-close"));
        return;
    }
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    state::set_signal(OPEN_MASK_SIGNAL, open & !bit);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    state::set_signal(MINIMIZED_MASK_SIGNAL, minimized & !bit);
    if state::get_signal(APP_SIGNAL) == id {
        state::set_signal(APP_SIGNAL, 0);
    }
    push_toast(&toast_named("toast-closed", id));
    remember_windows();
}

fn toggle_window_maximize(id: i64) {
    let (width, height) = stage_bounds();
    if !windows::toggle_maximize(id, width, height) {
        push_toast(&locale::t("toast-no-max"));
        return;
    }
    focus_app(id);
    let key = if windows::is_maximized(id) {
        "toast-max"
    } else {
        "toast-restore"
    };
    push_toast(&toast_named(key, id));
}

fn stage_bounds() -> (f32, f32) {
    let (viewport_w, viewport_h) = state::shell_viewport();
    let mut width = viewport_w;
    if state::get_signal(TASKS_SIGNAL) != 0 {
        width -= 300.0;
    }
    if state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0 {
        width -= 320.0;
    }
    // Desktop windows sit in the stage. Maximize fills that stage and stops
    // above the intent bar. Phone and tablet are full-bleed surfaces, so they
    // do not use this height for a floating frame.
    let height = if posture() == Posture::Desktop {
        (intent_block_top(viewport_h) - 8.0).max(200.0)
    } else {
        (viewport_h - 84.0).max(200.0)
    };
    (width.max(320.0), height)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Posture {
    Phone,
    Tablet,
    WideTablet,
    Desktop,
}

fn posture() -> Posture {
    let (width, _) = state::shell_viewport();
    if width < 600.0 {
        Posture::Phone
    } else if width < 1024.0 {
        Posture::Tablet
    } else if state::pointer_is_coarse() {
        Posture::WideTablet
    } else {
        Posture::Desktop
    }
}

fn ensure_clock_timer() {
    if CLOCK_ARMED.with(|armed| armed.replace(true)) {
        return;
    }
    let _ = w3cos_runtime::timers::set_interval(
        EventAction::Invoke {
            code: OP_CLOCK,
            input_placeholder: String::new(),
        },
        1000,
    );
    // Wake the window once a second. Xvfb can sit in Wait and never deliver
    // the timer deadline on its own.
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            w3cos_runtime::uitest::poke_event_loop();
        }
    });
}

fn on_shell_key(name: &str) -> bool {
    ensure_shell_signals();
    if ime_bar::on_key(name) {
        apply_ime_field();
        if let Some(key) = ime_bar::take_ink_toast() {
            push_toast(&locale::t(key));
        }
        return true;
    }
    match name {
        "Escape" => dismiss_top_layer(),
        "Mod+K" => {
            state::request_focus_placeholder(&locale::t("intent-placeholder"));
            true
        }
        _ => false,
    }
}

fn dismiss_top_layer() -> bool {
    if state::get_signal(UNLOCKED_SIGNAL) == 0 {
        return false;
    }
    if state::get_signal(BROADCAST_SIGNAL) != 0 {
        with_session(deny_broadcast);
        return true;
    }
    if state::get_signal(SANDBOX_SIGNAL) != 0 {
        with_session(deny_sandbox);
        return true;
    }
    if state::get_signal(SIGN_SIGNAL) != 0 {
        with_session(deny_sign_permission);
        return true;
    }
    if state::get_signal(CHAIN_SIGNAL) != 0 {
        with_session(deny_chain_read);
        return true;
    }
    if state::get_signal(PERM_SIGNAL) != 0 {
        state::set_signal(PERM_SIGNAL, 0);
        restore_after_dialog();
        return true;
    }
    if state::get_signal(PHASE_SIGNAL) == 1 {
        on_shell_intent(OP_CANCEL, "");
        return true;
    }
    if state::get_signal(NOTIFY_SIGNAL) != 0 {
        state::set_signal(NOTIFY_SIGNAL, 0);
        return true;
    }
    if state::get_signal(SWITCHER_SIGNAL) != 0 {
        state::set_signal(SWITCHER_SIGNAL, 0);
        return true;
    }
    if state::get_signal(LAUNCHER_SIGNAL) != 0 {
        state::set_signal(LAUNCHER_SIGNAL, 0);
        return true;
    }
    if state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0 {
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 0);
        return true;
    }
    false
}

/// After the top dialog closes, focus the control that is actually left.
/// Permission still open keeps its own focus. A confirm card underneath
/// gets 「确认」 or 「取消」. Otherwise the intent submit button, which the
/// next layout pass can focus. The intent field itself is a text input;
/// label focus only matches buttons.
fn restore_after_dialog() {
    if state::get_signal(SIGN_SIGNAL) != 0
        || state::get_signal(BROADCAST_SIGNAL) != 0
        || state::get_signal(PERM_SIGNAL) != 0
        || state::get_signal(CHAIN_SIGNAL) != 0
        || state::get_signal(SANDBOX_SIGNAL) != 0
    {
        return;
    }
    if state::get_signal(PHASE_SIGNAL) == 1 {
        let key = if state::get_signal_text(CONFIRM_FOCUS_SIGNAL) == "cancel" {
            "cancel"
        } else {
            "confirm"
        };
        state::request_focus_label(&locale::t(key));
        return;
    }
    state::request_focus_label(&locale::t("propose"));
}

fn on_shell_swipe(x0: f32, y0: f32, dx: f32, dy: f32) -> bool {
    ensure_shell_signals();
    let (width, height) = state::shell_viewport();
    let action = gesture::classify(
        x0,
        y0,
        dx,
        dy,
        width,
        height,
        matches!(posture(), Posture::Phone),
        state::get_signal(PHASE_SIGNAL) == 1,
        state::get_signal(PERM_SIGNAL) != 0
            || state::get_signal(SIGN_SIGNAL) != 0
            || state::get_signal(BROADCAST_SIGNAL) != 0
            || state::get_signal(CHAIN_SIGNAL) != 0
            || state::get_signal(SANDBOX_SIGNAL) != 0,
        state::get_signal(SWITCHER_SIGNAL) != 0,
    );
    match action {
        gesture::Swipe::None => false,
        gesture::Swipe::EdgeIgnored => true,
        gesture::Swipe::Switcher => {
            state::set_signal(SWITCHER_SIGNAL, 1);
            true
        }
        gesture::Swipe::Notifications => {
            state::set_signal(NOTIFY_SIGNAL, 1);
            state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
            true
        }
        gesture::Swipe::EdgeBack => edge_back(),
        gesture::Swipe::CloseFront => {
            let focused = state::get_signal(APP_SIGNAL);
            if focused != 0 {
                close_window(focused);
            }
            state::set_signal(SWITCHER_SIGNAL, 0);
            true
        }
    }
}

fn edge_back() -> bool {
    if state::get_signal(PERM_SIGNAL) != 0
        || state::get_signal(SIGN_SIGNAL) != 0
        || state::get_signal(BROADCAST_SIGNAL) != 0
        || state::get_signal(CHAIN_SIGNAL) != 0
        || state::get_signal(SANDBOX_SIGNAL) != 0
        || state::get_signal(PHASE_SIGNAL) == 1
    {
        return false;
    }
    if state::get_signal(SWITCHER_SIGNAL) != 0 {
        state::set_signal(SWITCHER_SIGNAL, 0);
        return true;
    }
    if state::get_signal(NOTIFY_SIGNAL) != 0 {
        state::set_signal(NOTIFY_SIGNAL, 0);
        return true;
    }
    if state::get_signal(LAUNCHER_SIGNAL) != 0 {
        state::set_signal(LAUNCHER_SIGNAL, 0);
        return true;
    }
    if state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0 {
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 0);
        return true;
    }
    if state::get_signal(APP_SIGNAL) != 0 {
        focus_app(0);
        return true;
    }
    false
}

fn remember_windows() {
    windows::save(
        state::get_signal(OPEN_MASK_SIGNAL),
        state::get_signal(MINIMIZED_MASK_SIGNAL),
        state::get_signal(APP_SIGNAL),
    );
}

fn on_window_drag(id: i64, mode: u8, dx: f32, dy: f32, phase: u8) {
    if phase == 1 {
        let bit = app_bit(id);
        if bit != 0 {
            state::set_signal(APP_SIGNAL, id);
            let open = state::get_signal(OPEN_MASK_SIGNAL);
            state::set_signal(OPEN_MASK_SIGNAL, open | bit);
            let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
            state::set_signal(MINIMIZED_MASK_SIGNAL, minimized & !bit);
        }
        windows::raise(id);
    }
    let (width, height) = stage_bounds();
    windows::on_drag(id, mode, dx, dy, phase, width, height);
    if phase == 2 {
        remember_windows();
    }
}

fn cycle_windows() {
    let current = state::get_signal(APP_SIGNAL);
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    let start = if (1..=6).contains(&current) {
        current
    } else {
        0
    };
    let mut found = None;
    for step in 1..=6 {
        let id = (start + step - 1) % 6 + 1;
        if resident(id) && open & app_bit(id) != 0 {
            found = Some(id);
            break;
        }
    }
    match found {
        Some(id) if id != current => {
            focus_app(id);
            push_toast(&toast_named("toast-switched", id));
        }
        Some(_) => push_toast(&locale::t("toast-no-other")),
        None => push_toast(&locale::t("toast-none")),
    }
}

fn handle_chrome_op(code: u8, draft: &str) -> bool {
    match code {
        OP_MINIMIZE => minimize_window(window_target(draft)),
        OP_CLOSE => close_window(window_target(draft)),
        OP_MAXIMIZE => toggle_window_maximize(window_target(draft)),
        OP_CYCLE => cycle_windows(),
        OP_LAUNCHER => {
            let next = i64::from(state::get_signal(LAUNCHER_SIGNAL) == 0);
            state::set_signal(LAUNCHER_SIGNAL, next);
        }
        OP_LAUNCHER_SEARCH => {
            state::set_text_signal(LAUNCHER_QUERY_SIGNAL, draft.trim());
            state::set_signal(LAUNCHER_SIGNAL, 1);
        }
        OP_DISMISS_TOAST => state::set_text_signal(TOAST_SIGNAL, ""),
        OP_SETTINGS_LOCAL => show_settings_pane(0),
        OP_SETTINGS_IDENTITY => show_settings_pane(1),
        OP_SETTINGS_BRIDGE => show_settings_pane(2),
        OP_SETTINGS_L2 => show_settings_pane(3),
        OP_SETTINGS_LANGUAGE => show_settings_pane(6),
        OP_SETTINGS_BACK => state::set_signal(SETTINGS_MENU_SIGNAL, 1),
        OP_FILES_HOME => state::set_signal(FILES_FOLDER_SIGNAL, 0),
        OP_FILES_DOCS => state::set_signal(FILES_FOLDER_SIGNAL, 1),
        OP_FILES_PICS => state::set_signal(FILES_FOLDER_SIGNAL, 2),
        OP_FILES_DOWN => state::set_signal(FILES_FOLDER_SIGNAL, 3),
        OP_FILES_MUSIC => state::set_signal(FILES_FOLDER_SIGNAL, 4),
        OP_FILES_PROJECTS => state::set_signal(FILES_FOLDER_SIGNAL, 5),
        OP_FILES_W3COS => state::set_signal(FILES_FOLDER_SIGNAL, 6),
        OP_TOGGLE_RECEIPTS => {
            let next = i64::from(state::get_signal(RECEIPTS_OPEN_SIGNAL) == 0);
            state::set_signal(RECEIPTS_OPEN_SIGNAL, next);
        }
        OP_FOCUS_DESKTOP => focus_app(0),
        OP_FOCUS_FILES => focus_app(1),
        OP_FOCUS_TERMINAL => focus_app(2),
        OP_FOCUS_SETTINGS => focus_app(3),
        OP_FOCUS_AGENT => open_task_center(),
        OP_FOCUS_BROWSER => focus_app(5),
        OP_FOCUS_EDITOR => focus_app(6),
        OP_LOCALE_ZH => stage_locale("zh-CN"),
        OP_LOCALE_EN => stage_locale("en"),
        OP_NOTIFY_TOGGLE => {
            let next = i64::from(state::get_signal(NOTIFY_SIGNAL) == 0);
            state::set_signal(NOTIFY_SIGNAL, next);
            if next != 0 {
                state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
            }
        }
        OP_NOTIFY_OPEN => open_notice_target(draft),
        OP_RECEIPT_PREV => {
            let page = state::get_signal(RECEIPT_PAGE_SIGNAL).saturating_sub(1);
            state::set_signal(RECEIPT_PAGE_SIGNAL, page);
        }
        OP_TRAY_L2 => {
            focus_app(3);
            show_settings_pane(3);
        }
        OP_RECEIPT_NEXT => {
            let pages = receipt_page_count(visible_receipts().len());
            let page = (state::get_signal(RECEIPT_PAGE_SIGNAL) + 1).min(pages - 1);
            state::set_signal(RECEIPT_PAGE_SIGNAL, page);
        }
        OP_CLOCK => {
            if handoff::note_clock() || handoff::poll_transport() {
                state::mark_dirty();
            }
            let now = live_clock();
            let changed = LAST_CLOCK.with(|slot| {
                let mut shown = slot.borrow_mut();
                if *shown == now {
                    false
                } else {
                    *shown = now;
                    true
                }
            });
            if changed {
                state::mark_dirty();
            }
        }
        OP_FILTER_ALL => set_receipt_filter(0),
        OP_FILTER_ALLOWED => set_receipt_filter(1),
        OP_FILTER_DENIED => set_receipt_filter(2),
        OP_FILTER_CANCELLED => set_receipt_filter(3),
        OP_RECEIPT_QUERY => {
            state::set_text_signal(RECEIPT_QUERY_SIGNAL, draft.trim());
            state::set_signal(RECEIPT_PAGE_SIGNAL, 0);
        }
        OP_COPY_SUBJECT => copy_subject(),
        OP_PREVIEW => {
            state::set_text_signal(FILE_PREVIEW_SIGNAL, draft.trim());
        }
        OP_SWITCHER => {
            let next = i64::from(state::get_signal(SWITCHER_SIGNAL) == 0);
            state::set_signal(SWITCHER_SIGNAL, next);
        }
        OP_SETTINGS_DEVICES => {
            focus_app(3);
            show_settings_pane(4);
        }
        OP_SETTINGS_LEXICON => {
            focus_app(3);
            show_settings_pane(5);
        }
        OP_LEXICON_STEALTH => {
            if lexicon::unlocked() {
                lexicon::toggle_stealth();
            } else {
                push_toast(&locale::t("lexicon-locked"));
            }
            state::mark_dirty();
        }
        OP_LEXICON_APP => {
            if lexicon::unlocked() {
                lexicon::toggle_app(app_key(state::get_signal(APP_SIGNAL)));
            } else {
                push_toast(&locale::t("lexicon-locked"));
            }
            state::mark_dirty();
        }
        OP_LEXICON_SAVE => {
            state::set_text_signal(LEXICON_DRAFT_SIGNAL, draft.trim());
            if lexicon::save_line(draft) {
                push_toast(&locale::t("lexicon-saved"));
            } else if !lexicon::unlocked() {
                push_toast(&locale::t("lexicon-locked"));
            }
            state::mark_dirty();
        }
        OP_LEXICON_DROP => {
            if lexicon::remove_line(draft) {
                push_toast(&locale::t("lexicon-removed"));
            } else if !lexicon::unlocked() {
                push_toast(&locale::t("lexicon-locked"));
            }
            state::mark_dirty();
        }
        OP_LEXICON_EXPORT => {
            if let Some(json) = lexicon::export_json() {
                handoff::publish_clipboard(&json);
                push_toast(&locale::t("lexicon-exported"));
            } else {
                push_toast(&locale::t("lexicon-locked"));
            }
        }
        OP_LEXICON_CLEAR => {
            if lexicon::clear() {
                push_toast(&locale::t("lexicon-cleared"));
            } else {
                push_toast(&locale::t("lexicon-locked"));
            }
            state::mark_dirty();
        }
        OP_PAIR_NO => {
            handoff::codes_differ();
            state::mark_dirty();
        }
        OP_PAIR_YES => {
            let _ = handoff::codes_match();
            state::mark_dirty();
        }
        OP_PAIR_IMPORT => {
            if handoff::import_pairing_payload(draft) {
                state::mark_dirty();
            }
        }
        OP_SYNC_RECEIPTS => {
            handoff::allow_receipt_sync();
            state::mark_dirty();
        }
        OP_SYNC_DRAFTS => {
            handoff::allow_draft_sync();
            state::mark_dirty();
        }
        OP_SYNC_CLIPBOARD => {
            handoff::allow_clipboard_sync();
            push_toast(&locale::t("toast-clipboard-sync"));
            state::mark_dirty();
        }
        OP_SYNC_ALLOW => {
            let gating = handoff::scene_gate_open();
            handoff::allow_current();
            if gating {
                send_handoff();
            }
            state::mark_dirty();
        }
        OP_SYNC_SKIP => {
            let gating = handoff::scene_gate_open();
            handoff::skip_current();
            if gating {
                send_handoff();
            }
            state::mark_dirty();
        }
        OP_HANDOFF_LATER => {
            handoff::hide_banner();
            state::mark_dirty();
        }
        OP_HANDOFF_GO => {
            if handoff::request_continue() {
                send_handoff();
            }
            state::mark_dirty();
        }
        OP_HANDOFF_CANCEL => {
            handoff::cancel_send();
            state::mark_dirty();
        }
        OP_HANDOFF_REVEAL => {
            handoff::reveal_incoming();
            state::mark_dirty();
        }
        OP_CLIP_MINE => {
            if let Some(text) = handoff::choose_clipboard(false) {
                handoff::publish_clipboard(&text);
            }
            state::mark_dirty();
        }
        OP_CLIP_THEIRS => {
            if let Some(text) = handoff::choose_clipboard(true) {
                handoff::publish_clipboard(&text);
            }
            state::mark_dirty();
        }
        OP_HANDOFF_OPEN => {
            focus_app(1);
            handoff::dismiss_incoming();
            push_toast(&locale::t("handoff-opened"));
        }
        OP_HANDOFF_HIDE => {
            handoff::dismiss_incoming();
            state::mark_dirty();
        }
        OP_SCRIM => {}
        OP_IME_1 | OP_IME_2 | OP_IME_3 | OP_IME_4 | OP_IME_5 | OP_IME_6 | OP_IME_7 | OP_IME_8
        | OP_IME_9 => {
            ime_bar::commit_candidate((code - OP_IME_1) as usize);
            apply_ime_field();
            if let Some(key) = ime_bar::take_ink_toast() {
                push_toast(&locale::t(key));
            }
            if let Some(key) = ime_bar::take_screen_toast() {
                push_toast(&locale::t(key));
            }
        }
        OP_IME_MODE => {
            ime_bar::toggle_mode();
            apply_ime_field();
        }
        OP_IME_PAGE_PREV => {
            ime_bar::page_by(-1);
        }
        OP_IME_PAGE_NEXT => {
            ime_bar::page_by(1);
        }
        OP_IME_VOICE => push_toast(&locale::t(ime_bar::voice_tap_key())),
        OP_IME_MODEL => {
            push_toast(&locale::t(ime_bar::confirm_speech_model()));
            state::mark_dirty();
        }
        OP_IME_INK_MODEL => {
            push_toast(&locale::t(ime_bar::confirm_handwriting_model()));
            state::mark_dirty();
        }
        OP_IME_INK => {
            if let Some(key) = ime_bar::ink_tap() {
                push_toast(&locale::t(key));
            }
            state::mark_dirty();
        }
        OP_IME_INK_CLEAR => ime_bar::clear_ink(),
        OP_IME_SCREEN => {
            let key = ime_bar::screen_tap();
            flush_screen_decision();
            if let Some(key) = key {
                push_toast(&locale::t(key));
            }
            state::mark_dirty();
        }
        OP_IME_SCREEN_YES => {
            let key = ime_bar::screen_allow();
            flush_screen_decision();
            if let Some(key) = key {
                push_toast(&locale::t(key));
            }
            state::mark_dirty();
        }
        OP_IME_SCREEN_NO => {
            let key = ime_bar::screen_cancel();
            flush_screen_decision();
            if let Some(key) = key {
                push_toast(&locale::t(key));
            }
            state::mark_dirty();
        }
        OP_IME_SCREEN_MODEL => {
            push_toast(&locale::t(ime_bar::confirm_screen_model()));
            state::mark_dirty();
        }
        OP_IME_CLIPBOARD => {
            let text = match ime_bar::local_clipboard_gate() {
                ime_bar::ClipboardGate::Read => read_clipboard(),
                ime_bar::ClipboardGate::Secure | ime_bar::ClipboardGate::Closed => None,
            };
            let tap = ime_bar::accept_local_clipboard(text.as_deref());
            let wrote = tap.wrote();
            push_toast(&locale::t(tap.toast_key()));
            if wrote {
                apply_ime_field();
            }
        }
        OP_SOFTKEY => match ime_bar::on_soft_key(draft) {
            ime_bar::SoftOutcome::Edit => apply_ime_field(),
            ime_bar::SoftOutcome::Submit { role, text } => {
                apply_ime_field();
                let code = match role.as_str() {
                    "intent" => OP_PROPOSE,
                    "terminal" => OP_RUN_TERMINAL,
                    "launcher" => OP_LAUNCHER_SEARCH,
                    "text" => OP_RECEIPT_QUERY,
                    "secure" => OP_UNLOCK,
                    _ => return true,
                };
                on_shell_intent(code, &text);
            }
            ime_bar::SoftOutcome::None => {}
        },
        _ => return false,
    }
    true
}

fn l2_env_configured() -> bool {
    matches!(
        std::env::var("W3COS_L2").unwrap_or_default().trim(),
        "1" | "true" | "script"
    )
}

fn toggle_l2(session: &mut IntentSession) {
    if session.l2_enabled() {
        session.set_l2_enabled(false);
        if state::get_signal(PHASE_SIGNAL) == 3 || w3cos_ai_bridge::agent_loop::has_pending() {
            let update = agent_host::cancel();
            apply_host_update(session, update);
        }
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("toast-l2-off"),
        );
        return;
    }
    if l2_env_configured() {
        session.set_l2_enabled(true);
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("toast-l2-on"),
        );
        return;
    }
    sync_session(
        session,
        state::get_signal(PHASE_SIGNAL),
        &locale::t("toast-l2-blocked"),
    );
}

fn toggle_infer_backend(session: &mut IntentSession) {
    use w3cos_ai_bridge::infer::{self, InferBackendKind};
    let next = if infer::active_backend() == InferBackendKind::InProcess {
        InferBackendKind::Inferd
    } else {
        InferBackendKind::InProcess
    };
    let notice = if infer::set_runtime_backend(next).is_ok() {
        if next == InferBackendKind::InProcess {
            locale::t("toast-infer-in-process")
        } else {
            locale::t("toast-infer-inferd")
        }
    } else {
        locale::t("infer-backend-unavailable")
    };
    sync_session(session, state::get_signal(PHASE_SIGNAL), &notice);
}

fn session_l2_on() -> bool {
    SESSION.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|session| session.l2_enabled())
    })
}

fn present_settings(tab: i64, menu: i64) {
    focus_app(3);
    show_settings_pane(tab);
    state::set_signal(SETTINGS_MENU_SIGNAL, menu);
    let (width, height) = stage_bounds();
    if !windows::is_maximized(3) {
        let _ = windows::toggle_maximize(3, width, height);
    }
}

fn screenshot_shows_locked_permission() -> bool {
    matches!(
        std::env::var("W3COS_SCREEN").ok().as_deref(),
        Some("permission-locked") | Some("sign-permission-locked")
    )
}

fn apply_screen_override() {
    // Screenshot hook. Set once so a later click can still change the screen.
    thread_local! {
        static APPLIED: Cell<bool> = const { Cell::new(false) };
    }
    if APPLIED.with(|applied| applied.replace(true)) {
        return;
    }
    if let Ok(query) = std::env::var("W3COS_LAUNCHER_QUERY") {
        state::set_text_signal(LAUNCHER_QUERY_SIGNAL, query.trim());
    }
    match std::env::var("W3COS_SCREEN").ok().as_deref() {
        Some("settings") => present_settings(0, 1),
        Some("settings-night") => {
            present_settings(0, 0);
            state::set_signal(NIGHT_SIGNAL, 1);
        }
        Some("settings-l2") => present_settings(3, 0),
        Some("settings-identity") => present_settings(1, 0),
        Some("settings-bridge") | Some("settings-agent") => present_settings(2, 0),
        Some("settings-language") => present_settings(6, 0),
        Some("files") => focus_app(1),
        Some("closed") => {
            focus_app(1);
            close_window(1);
        }
        Some("files-docs") => {
            focus_app(1);
            state::set_signal(FILES_FOLDER_SIGNAL, 1);
        }
        Some("terminal") => focus_app(2),
        Some("terminal-echo") => {
            focus_app(2);
            state::set_text_signal(TERMINAL_SIGNAL, "echo 已运行");
            with_session(|session| {
                if session.identity_unlocked() {
                    run_terminal_from_button(session);
                }
            });
        }
        Some("terminal-refused") | Some("sandbox") => {
            focus_app(2);
            state::set_text_signal(TERMINAL_SIGNAL, "ls");
            with_session(|session| {
                if session.identity_unlocked() {
                    open_sandbox_permission(session);
                }
            });
        }
        Some("launcher") => state::set_signal(LAUNCHER_SIGNAL, 1),
        Some("receipts") => {
            state::set_signal(RECEIPTS_OPEN_SIGNAL, 1);
            state::set_text_signal(
                RECEIPT_SIGNAL,
                "#4 allowed open-files tier=L0 actor=did:key:z6MkhaXgExample confirmed=true ts=1760518800000 — 已切换到文件 @@ 打开文件\n\
                 #3 unrecognized summarize tier=L2 actor=did:key:z6MkOLD confirmed=false ts=1760515200000 — 没有对应命令 @@ 把这段总结一下\n\
                 #2 cancelled night-mode tier=L1 actor=did:key:z6MkhaXgExample confirmed=false ts=1760511600000 — 你取消了，没有执行 @@ 夜间模式\n\
                 #1 denied echo tier=L0 actor=did:key:z6MkhaXgExample confirmed=true ts=1760508000000 — 当时还锁着 @@ 在终端运行 echo",
            );
        }
        Some("receipts-empty") => {
            state::set_signal(RECEIPTS_OPEN_SIGNAL, 1);
            state::set_text_signal(RECEIPT_SIGNAL, "");
        }
        Some("tool-confirm") => show_tool_confirm("a11y_index"),
        Some("http-confirm") => show_tool_confirm("click"),
        Some("host-confirm") => show_host_confirm(),
        Some("chain-read") => show_chain_read(),
        Some("tx-sign") => {
            present_settings(1, 0);
            state::set_text_signal(CHAIN_RPC_SIGNAL, "http://127.0.0.1:8545");
            with_session(open_tx_permission);
        }
        Some("tx-broadcast") => {
            present_settings(1, 0);
            state::set_text_signal(CHAIN_RPC_SIGNAL, "http://127.0.0.1:8545");
            with_session(|session| {
                if session.identity_unlocked() {
                    open_tx_permission(session);
                    accept_sign_permission(session);
                }
            });
        }
        Some("passkey") => {
            present_settings(1, 0);
            with_session(|session| {
                if !session.identity_unlocked() {
                    session.unlock_with_passphrase("w3cos-local");
                }
                let _ = enroll_device_passkey(session);
                sync_session(session, 0, "");
            });
        }
        Some("confirm") => {
            focus_app(1);
            state::set_signal(PHASE_SIGNAL, 1);
            state::set_text_signal(TIER_SIGNAL, "L0");
            state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "confirm");
            state::set_text_signal(SUMMARY_SIGNAL, "打开文件");
            state::set_text_signal(
                IMPACT_SIGNAL,
                "切换到文件窗口。不读取、不修改磁盘上的内容。",
            );
            state::request_focus_label(&locale::t("confirm"));
        }
        Some("permission") => {
            focus_app(2);
            state::set_text_signal(TERMINAL_SIGNAL, "echo");
            state::set_signal(PERM_SIGNAL, 1);
            state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
            state::request_focus_label(&locale::t("perm-deny"));
        }
        Some("permission-locked") => {
            focus_app(2);
            state::set_text_signal(TERMINAL_SIGNAL, "echo");
            state::set_signal(PERM_SIGNAL, 1);
            state::set_signal(UNLOCKED_SIGNAL, 0);
            state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "");
        }
        Some("sign-permission") => {
            present_settings(1, 0);
            with_session(|session| open_sign_permission(session, "hello from this device"));
        }
        Some("sign-permission-locked") => {
            present_settings(1, 0);
            with_session(|session| open_sign_permission(session, "hello from this device"));
            state::set_signal(UNLOCKED_SIGNAL, 0);
        }
        Some("receipts-signed") => {
            with_session(|session| {
                if session.identity_unlocked() {
                    open_sign_permission(session, "hello from this device");
                    accept_sign_permission(session);
                }
            });
            state::set_signal(SIGN_SIGNAL, 0);
            state::set_signal(RECEIPTS_OPEN_SIGNAL, 1);
            state::set_text_signal(TOAST_SIGNAL, "");
            state::set_text_signal(NOTICE_SIGNAL, "");
        }
        Some("lock") => state::set_signal(UNLOCKED_SIGNAL, 0),
        Some("switcher") => {
            focus_app(1);
            state::set_signal(SWITCHER_SIGNAL, 1);
        }
        Some("notify") => {
            state::set_signal(NOTIFY_SIGNAL, 1);
            state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
        }
        Some("notify-files") => {
            let ctx = ShellContext {
                app: ShellApp::Desktop,
                night: state::get_signal(NIGHT_SIGNAL) != 0,
            };
            with_session(|session| {
                if session.identity_unlocked() {
                    apply_propose(session, "打开文件", &ctx);
                    apply_confirm(session, &ctx);
                }
            });
            state::set_signal(NOTIFY_SIGNAL, 1);
            state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
            state::set_signal(PHASE_SIGNAL, 0);
        }
        Some("notify-badge") => {
            let ctx = ShellContext {
                app: ShellApp::Desktop,
                night: state::get_signal(NIGHT_SIGNAL) != 0,
            };
            with_session(|session| {
                if session.identity_unlocked() {
                    apply_propose(session, "打开文件", &ctx);
                    apply_confirm(session, &ctx);
                }
            });
            state::set_signal(NOTIFY_SIGNAL, 0);
            state::set_signal(PHASE_SIGNAL, 0);
        }
        Some("devices") => {
            present_settings(4, 0);
            handoff::demo_pending();
        }
        Some("devices-sync") => {
            present_settings(4, 0);
            handoff::demo_pending();
            let _ = handoff::codes_match();
        }
        Some("handoff") => {
            focus_app(1);
            handoff::demo_banner();
        }
        Some("handoff-sending") => {
            focus_app(1);
            handoff::demo_sending();
        }
        Some("handoff-no-answer") => {
            focus_app(1);
            handoff::demo_no_answer();
        }
        Some("handoff-phone") => {
            focus_app(0);
            handoff::demo_incoming();
        }
        Some("lexicon") => {
            focus_app(3);
            show_settings_pane(5);
            lexicon::seed_demo();
        }
        Some("lexicon-rank") => {
            focus_app(0);
            lexicon::seed_demo();
            state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
                index: 4,
                value: String::new(),
                role: "intent".to_string(),
                secure: false,
                x: 280.0,
                y: 700.0,
                width: 640.0,
                height: 36.0,
            });
            ime_bar::demo_compose("nihao");
        }
        Some("windows") => {
            state::set_signal(OPEN_MASK_SIGNAL, app_bit(1) | app_bit(2) | app_bit(5));
            state::set_signal(MINIMIZED_MASK_SIGNAL, app_bit(5));
            focus_app(1);
            push_toast(&locale::t("toast-browser-min"));
        }
        Some("tasks") => state::set_signal(TASKS_SIGNAL, 1),
        Some("ime-smart")
        | Some("ime-voice")
        | Some("ime-ink")
        | Some("ime-ink-pad")
        | Some("ime-screen")
        | Some("ime-screen-model")
        | Some("ime-screen-ask")
        | Some("ime-screen-region") => {
            focus_app(0);
            state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
                index: 4,
                value: String::new(),
                role: "intent".into(),
                secure: false,
                x: 80.0,
                y: 180.0,
                width: 280.0,
                height: 36.0,
            });
            match std::env::var("W3COS_SCREEN").ok().as_deref() {
                Some("ime-voice") => push_toast(&locale::t("ime-toast-voice")),
                Some("ime-ink") => {
                    if let Some(key) = ime_bar::ink_tap() {
                        push_toast(&locale::t(key));
                    }
                }
                Some("ime-ink-pad") => ime_bar::show_ink_pad_for_screen(),
                Some("ime-screen") => {
                    if let Some(key) = ime_bar::screen_tap() {
                        push_toast(&locale::t(key));
                    }
                }
                Some("ime-screen-model") => {
                    present_settings(6, 0);
                    ime_bar::arm_screen_model_confirm_for_screen();
                }
                Some("ime-screen-ask") => ime_bar::show_screen_ask_for_screen(),
                Some("ime-screen-region") => ime_bar::show_screen_region_for_screen(),
                _ => {}
            }
        }
        _ => {}
    }
}

fn install_ime_hooks() {
    state::set_shell_hold_hook(on_shell_hold);
    state::set_shell_poll_hook(on_shell_poll);
    state::set_shell_ink_hook(on_shell_ink);
}

fn on_shell_ink(phase: u8, x: f32, y: f32) {
    if ime_bar::screen_drag_active() {
        ime_bar::screen_pointer(phase, x, y);
        flush_screen_decision();
        if let Some(key) = ime_bar::take_screen_toast() {
            push_toast(&locale::t(key));
        }
        return;
    }
    ime_bar::ink_pointer(phase, x, y);
}

fn on_shell_hold(code: u8, phase: u8) {
    if code != OP_IME_VOICE {
        return;
    }
    match phase {
        0 => match ime_bar::voice_press() {
            w3cos_ime::SpeechStart::Listening => {
                push_toast(&locale::t("ime-toast-voice-live"));
                apply_ime_field();
            }
            w3cos_ime::SpeechStart::Off(reason) => {
                push_toast(&locale::t(ime_bar::voice_off_key(reason)));
            }
        },
        2 => match ime_bar::voice_release() {
            ime_bar::VoiceRelease::Wrote => {
                push_toast(&locale::t("ime-toast-voice-ok"));
                apply_ime_field();
            }
            ime_bar::VoiceRelease::Empty => {
                push_toast(&locale::t("ime-toast-voice-empty"));
                apply_ime_field();
            }
            ime_bar::VoiceRelease::Idle => {}
        },
        _ => {}
    }
}

fn on_shell_poll() {
    flush_screen_decision();
    if ime_bar::poll_screen() {
        if let Some(key) = ime_bar::take_screen_toast() {
            push_toast(&locale::t(key));
        }
    }
    if ime_bar::poll_voice() {
        apply_ime_field();
    }
}

fn flush_screen_decision() {
    let Some(decision) = ime_bar::take_screen_decision() else {
        return;
    };
    let (decision, outcome) = match decision {
        ime_bar::ScreenDecision::Allowed => {
            (Decision::Allowed, locale::t("ime-screen-receipt-allowed"))
        }
        ime_bar::ScreenDecision::Cancelled => (
            Decision::Cancelled,
            locale::t("ime-screen-receipt-cancelled"),
        ),
    };
    record_screen_receipt(decision, &outcome);
}

fn record_screen_receipt(decision: Decision, outcome: &str) {
    let utterance = locale::t("ime-screen-title");
    with_session(|session| {
        session.record_external(
            &utterance,
            "screen-region",
            decision,
            decision == Decision::Allowed,
            outcome,
            IntentTier::L0,
        );
        state::set_text_signal(RECEIPT_SIGNAL, session.ledger_log_tail(48));
    });
    let _ = absorb_new_receipts();
}

fn build_shell() -> Component {
    ensure_shell_signals();
    install_ime_hooks();
    let _ = handoff::poll_transport();
    if let Some(session) = windows::load_once() {
        let agent = app_bit(4);
        let retired =
            session.open & agent != 0 || session.minimized & agent != 0 || session.focused == 4;
        state::set_signal(OPEN_MASK_SIGNAL, session.open & !agent);
        state::set_signal(MINIMIZED_MASK_SIGNAL, session.minimized & !agent);
        let focused = if session.focused == 4 {
            0
        } else {
            session.focused.clamp(0, 6)
        };
        state::set_signal(APP_SIGNAL, focused);
        if retired {
            remember_windows();
        }
    }
    boot_session_once();
    observe_live_notices();
    apply_screen_override();
    ime_bar::sync_field_presence();
    let night = state::get_signal(NIGHT_SIGNAL) != 0;
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut column = match posture() {
        Posture::Phone => vec![build_phone_stack()],
        Posture::Tablet | Posture::WideTablet => vec![build_tablet_row()],
        Posture::Desktop => vec![build_stage(), floating_intent(), floating_taskbar()],
    };
    if state::get_signal(SWITCHER_SIGNAL) != 0 && matches!(posture(), Posture::Phone) {
        column.push(chrome_layer(build_switcher()));
    }
    if handoff::show_banner() && posture() != Posture::Phone {
        column.push(chrome_layer(build_handoff_banner()));
    }
    if handoff::incoming().is_some() {
        column.push(chrome_layer(build_handoff_incoming()));
    }
    if handoff::sync_prompt().is_some() {
        column.push(modal_layer(build_sync_sheet()));
    }
    if handoff::clip_conflict().is_some() && !clip_sheet_inside_incoming() {
        column.push(chrome_layer(build_clip_dock()));
    }
    if state::get_signal(TASKS_SIGNAL) != 0 {
        let panel = build_task_center();
        if posture() == Posture::Phone {
            column.push(chrome_layer(place_receipts(panel)));
        } else {
            column.push(dock_right(panel));
        }
    }
    if !state::get_signal_text(PENDING_LOCALE_SIGNAL).is_empty() {
        column.push(modal_layer(build_locale_confirm()));
    }
    if state::get_signal(PHASE_SIGNAL) == 1 {
        column.push(modal_layer(build_confirm_dialog()));
    }
    if state::get_signal(PERM_SIGNAL) != 0 {
        column.push(permission_layer(build_permission_sheet()));
    }
    if state::get_signal(SANDBOX_SIGNAL) != 0 {
        column.push(place_modal(build_sandbox_sheet(), false, 680.0));
    }
    if state::get_signal(SIGN_SIGNAL) != 0 {
        let tall = state::get_signal_text(SIGN_MESSAGE_SIGNAL).contains('\n');
        column.push(place_modal(
            build_sign_sheet(),
            false,
            if tall { 680.0 } else { 520.0 },
        ));
    }
    if state::get_signal(BROADCAST_SIGNAL) != 0 {
        column.push(place_modal(build_broadcast_sheet(), false, 680.0));
    }
    if state::get_signal(CHAIN_SIGNAL) != 0 {
        column.push(place_modal(build_chain_sheet(), false, 480.0));
    }
    if state::get_signal(NOTIFY_SIGNAL) != 0 {
        column.push(chrome_layer(build_notify_panel()));
    }
    if state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0 {
        column.push(place_receipts(build_receipts_viewer()));
    }
    if state::get_signal(LAUNCHER_SIGNAL) != 0 && posture() != Posture::Phone {
        column.push(chrome_layer(build_launcher()));
    }
    let toast = state::get_signal_text(TOAST_SIGNAL);
    if !toast.is_empty() && state::get_signal(NOTIFY_SIGNAL) == 0 {
        column.push(chrome_layer(build_toast(&toast)));
    }
    if let Some(bar) = ime_bar::modality_layer() {
        column.push(bar);
    }
    if let Some(bar) = ime_bar::candidate_layer() {
        column.push(bar);
    }
    if let Some(pad) = ime_bar::ink_pad_layer() {
        column.push(pad);
    }
    if let Some(screen) = ime_bar::screen_layer() {
        column.push(screen);
    }
    if !unlocked && !screenshot_shows_locked_permission() {
        column.push(place_overlay(build_lock_screen()));
    }
    if let Some(keyboard) = ime_bar::phone_keyboard() {
        column.push(keyboard);
    }

    let mut shell = Component::column(
        Style {
            width: Dimension::Vw(100.0),
            height: Dimension::Vh(100.0),
            position: Position::Relative,
            overflow: Overflow::Hidden,
            background: Color::from_hex(if night { "#15202c" } else { "#1a2836" }),
            gap: 0.0,
            direction: locale::text_direction(),
            ..Style::default()
        },
        column,
    );
    trap_modal_focus(&mut shell);
    shell
}

/// Tab only reaches buttons and text inputs. While a confirm card or
/// permission sheet is up, every other control becomes a plain container
/// so focus stays inside that surface. Clicks on those controls are kept.
/// The lock screen stays as-is: it is the surface in front.
fn trap_modal_focus(root: &mut Component) {
    if state::get_signal(UNLOCKED_SIGNAL) == 0 && !screenshot_shows_locked_permission() {
        return;
    }
    let role = if state::get_signal(BROADCAST_SIGNAL) != 0 {
        "broadcast"
    } else if state::get_signal(SANDBOX_SIGNAL) != 0 {
        "sandbox"
    } else if state::get_signal(SIGN_SIGNAL) != 0 {
        "sign"
    } else if state::get_signal(CHAIN_SIGNAL) != 0 {
        "chain"
    } else if state::get_signal(PERM_SIGNAL) != 0 {
        "permission"
    } else if state::get_signal(PHASE_SIGNAL) == 1 {
        "confirm"
    } else {
        return;
    };
    defocus_outside(root, role);
}

fn dialog_role(node: &Component) -> Option<&str> {
    node.style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get("--w3cos-dialog"))
        .map(String::as_str)
}

fn defocus_outside(node: &mut Component, role: &str) {
    if dialog_role(node) == Some(role) {
        return;
    }
    defocus_node(node);
    for child in &mut node.children {
        defocus_outside(child, role);
    }
}

fn defocus_node(node: &mut Component) {
    let painted = match &node.kind {
        ComponentKind::Button { label } if node.style.color.a > 0 && !label.trim().is_empty() => {
            Some(Component::text(
                label.clone(),
                Style {
                    font_size: node.style.font_size,
                    font_weight: node.style.font_weight,
                    color: node.style.color,
                    text_align: TextAlign::Center,
                    ..Style::default()
                },
            ))
        }
        ComponentKind::Button { .. } => None,
        ComponentKind::TextInput {
            value,
            placeholder,
            secure,
        } => {
            let empty = value.is_empty();
            let content = if *secure {
                "•".repeat(value.chars().count())
            } else if empty {
                placeholder.clone()
            } else {
                value.clone()
            };
            let color = if empty && !*secure {
                Color::rgb(107, 114, 128)
            } else {
                node.style.color
            };
            Some(Component::text(
                content,
                Style {
                    font_size: node.style.font_size,
                    font_weight: node.style.font_weight,
                    color,
                    flex_grow: 1.0,
                    ..Style::default()
                },
            ))
        }
        _ => return,
    };
    node.kind = ComponentKind::Column;
    if let Some(text) = painted {
        node.style.justify_content = JustifyContent::Center;
        node.style.align_items = AlignItems::Center;
        node.children.insert(0, text);
    }
}

fn mark_dialog(card: &mut Component, role: &str) {
    card.style
        .custom_properties
        .get_or_insert_with(HashMap::new)
        .insert("--w3cos-dialog".to_string(), role.to_string());
}

fn modal_layer(card: Component) -> Component {
    place_modal(card, false, 320.0)
}

fn permission_layer(card: Component) -> Component {
    place_modal(card, false, 420.0)
}

fn place_modal(mut card: Component, fill: bool, phone_sheet: f32) -> Component {
    let (width, height) = state::shell_viewport();
    let scrim_h = if fill || posture() == Posture::Phone {
        height
    } else {
        (height - 68.0).max(0.0)
    };
    // A column, not a button. It swallows clicks outside the card, and it
    // must not join the tab order or paint a ring around the desktop.
    let mut scrim = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(0.0),
            top: Dimension::Px(0.0),
            width: Dimension::Px(width),
            height: Dimension::Px(scrim_h),
            background: Color::rgba(6, 10, 14, 117),
            z_index: CHROME_Z + 2,
            ..Style::default()
        },
        vec![],
    );
    scrim.on_click = invoke(OP_SCRIM);
    card.style.position = Position::Absolute;
    card.style.z_index = CHROME_Z + 3;
    if fill {
        card.style.left = Dimension::Px(0.0);
        card.style.top = Dimension::Px(0.0);
        card.style.width = Dimension::Px(width);
        card.style.height = Dimension::Px(height);
    } else if posture() == Posture::Phone {
        let sheet_h = phone_sheet.min(height - 24.0);
        card.style.left = Dimension::Px(0.0);
        card.style.width = Dimension::Px(width);
        card.style.height = Dimension::Px(sheet_h);
        card.style.top = Dimension::Px((height - sheet_h).max(0.0));
    } else {
        let card_w = 440.0_f32.min(width - 32.0);
        card.style.width = Dimension::Px(card_w);
        card.style.left = Dimension::Px(((width - card_w) / 2.0).max(16.0));
        card.style.top = Dimension::Px((height * 0.22).max(32.0));
    }
    Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(0.0),
            top: Dimension::Px(0.0),
            width: Dimension::Px(width),
            height: Dimension::Px(height),
            z_index: CHROME_Z + 2,
            ..Style::default()
        },
        vec![scrim, card],
    )
}

fn place_overlay(mut node: Component) -> Component {
    let (width, height) = state::shell_viewport();
    node.style.position = Position::Absolute;
    node.style.left = Dimension::Px(0.0);
    node.style.top = Dimension::Px(0.0);
    node.style.width = Dimension::Px(width);
    node.style.height = Dimension::Px(height);
    node.style.z_index = CHROME_Z + 4;
    node
}

/// Top of the floating intent block. Drawers stop above this so they leave the
/// submit control uncovered.
fn intent_block_top(height: f32) -> f32 {
    (height - 84.0 - 78.0).max(8.0)
}

fn place_receipts(mut panel: Component) -> Component {
    let (width, height) = state::shell_viewport();
    panel.style.position = Position::Absolute;
    panel.style.z_index = CHROME_Z + 1;
    if posture() == Posture::Phone {
        let top = 56.0_f32.min((height * 0.22).max(48.0));
        panel.style.left = Dimension::Px(0.0);
        panel.style.top = Dimension::Px(top);
        panel.style.width = Dimension::Px(width);
        panel.style.height = Dimension::Px((height - top).max(240.0));
        panel.style.border_radius = 22.0;
        panel.style.border_top_left_radius = Some(22.0);
        panel.style.border_top_right_radius = Some(22.0);
        panel.style.border_bottom_left_radius = Some(0.0);
        panel.style.border_bottom_right_radius = Some(0.0);
        return panel;
    }
    let panel_w = 400.0_f32.min((width - 48.0).max(280.0));
    let top = 24.0;
    let bottom = (intent_block_top(height) - 12.0).max(top);
    panel.style.left = Dimension::Px((width - panel_w - 24.0).max(8.0));
    panel.style.top = Dimension::Px(top);
    panel.style.width = Dimension::Px(panel_w);
    panel.style.height = Dimension::Px(bottom - top);
    panel.style.border_radius = 16.0;
    panel
}

fn dock_right(panel: Component) -> Component {
    let (width, height) = state::shell_viewport();
    let mut docked = panel;
    docked.style.position = Position::Absolute;
    docked.style.left = Dimension::Px((width - 340.0).max(0.0));
    docked.style.top = Dimension::Px(0.0);
    docked.style.width = Dimension::Px(340.0_f32.min(width));
    docked.style.height = Dimension::Px(height);
    docked.style.z_index = CHROME_Z + 1;
    docked
}

const CHROME_Z: i32 = 1_000_000;

fn chrome_layer(mut component: Component) -> Component {
    if component.style.position == Position::Static {
        component.style.position = Position::Relative;
    }
    if component.style.z_index < CHROME_Z {
        component.style.z_index = CHROME_Z;
    }
    component
}

#[derive(Clone, Copy)]
struct ResidentApp {
    id: i64,
    glyph: &'static str,
    color: &'static str,
}

const RESIDENT_APPS: [ResidentApp; 5] = [
    ResidentApp {
        id: 1,
        glyph: "files",
        color: "#3498db",
    },
    ResidentApp {
        id: 2,
        glyph: "terminal",
        color: "#2ecc71",
    },
    ResidentApp {
        id: 3,
        glyph: "settings",
        color: "#d4a017",
    },
    ResidentApp {
        id: 5,
        glyph: "browser",
        color: "#f39c12",
    },
    ResidentApp {
        id: 6,
        glyph: "editor",
        color: "#1abc9c",
    },
];

fn resident(id: i64) -> bool {
    RESIDENT_APPS.iter().any(|app| app.id == id)
}

const WALLPAPER: &str = "radial-gradient(980px 560px at 12% -8%, rgba(120, 168, 186, 0.22), transparent 62%), radial-gradient(820px 520px at 100% 110%, rgba(20, 90, 72, 0.28), transparent 58%), linear-gradient(180deg, #1b3344 0%, #13202c 48%, #101820 100%)";

/// Labeled chrome (confirm, permission, propose, settings links).
/// Coarse pointers use 48px. Fine pointers use 44px.
/// Tray icons stay 40px and desktop title buttons stay 28px.
fn touch_min() -> f32 {
    if state::pointer_is_coarse() {
        48.0
    } else {
        44.0
    }
}

fn line_strong() -> Color {
    Color::rgba(244, 241, 234, 56)
}

fn ink_token() -> Color {
    Color::from_hex("#f4f1ea")
}

fn muted_token() -> Color {
    Color::from_hex("#c5d0d8")
}

fn faint_token() -> Color {
    Color::from_hex("#aebbc6")
}

fn shadow_sm() -> BoxShadow {
    BoxShadow::new(0.0, 8.0, 24.0, 0.0, Color::rgba(0, 0, 0, 82))
}

fn shadow_lg() -> BoxShadow {
    BoxShadow::new(0.0, 24.0, 60.0, 0.0, Color::rgba(0, 0, 0, 115))
}

fn inset_highlight(alpha: u8) -> BoxShadow {
    BoxShadow::inset(0.0, 1.0, 0.0, 0.0, Color::rgba(255, 255, 255, alpha))
}

fn label_shadow() -> Vec<TextShadow> {
    vec![TextShadow {
        offset_x: 0.0,
        offset_y: 1.0,
        blur_radius: 2.0,
        color: Color::rgba(0, 0, 0, 115),
    }]
}

fn sheet_fill() -> Color {
    Color::rgba(22, 30, 39, 245)
}

fn glyph_svg(name: &str, stroke: &str) -> String {
    let body = match name {
        "files" => {
            r#"<path d="M4 8.2A2 2 0 0 1 6 6.2h3.1L11 8.2h7a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8.2Z"/>"#
        }
        "terminal" => {
            r#"<rect x="4" y="5.5" width="16" height="13" rx="2"/><path d="M7.5 9.5 10 12l-2.5 2.5M12 14.5h4.5"/>"#
        }
        "settings" => {
            r#"<path d="M4 8h16M4 16h16"/><circle cx="9" cy="8" r="2.2"/><circle cx="15" cy="16" r="2.2"/>"#
        }
        "browser" => {
            r#"<circle cx="12" cy="12" r="7.5"/><path d="m14.6 9.4-1.6 3.6-3.6 1.6 1.6-3.6 3.6-1.6Z"/>"#
        }
        "editor" => {
            r#"<path d="M4.5 19.5h4L19 9a2.1 2.1 0 0 0-3-3L5.5 16.5v3Z"/><path d="M13.5 7.5l3 3"/>"#
        }
        "lock" => {
            r#"<rect x="6" y="10.5" width="12" height="9" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>"#
        }
        "lock-open" => {
            r#"<rect x="6" y="10.5" width="12" height="9" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 6.4-1.9"/>"#
        }
        "bell" => {
            r#"<path d="M6 16.5h12l-1.2-2V10.2a4.8 4.8 0 0 0-9.6 0v4.3L6 16.5Z"/><path d="M10 16.5a2 2 0 0 0 4 0"/>"#
        }
        "devices" => {
            r#"<rect x="3" y="6" width="12" height="8" rx="1.4"/><path d="M7 17.5h6M10 14v3.5"/><rect x="16" y="8" width="5" height="9" rx="1.2"/>"#
        }
        "tasks" => {
            r#"<path d="M9 7h10M9 12h10M9 17h10"/><path d="m4.5 7.2 1.2 1.2 2.1-2.2M4.5 12.2l1.2 1.2 2.1-2.2M4.5 17.2l1.2 1.2 2.1-2.2"/>"#
        }
        "arrow" => {
            r#"<path d="M19 7v5.2A2.8 2.8 0 0 1 16.2 15H6"/><path d="M9.5 11.5 6 15l3.5 3.5"/>"#
        }
        "minus" => r#"<path d="M6 12h12"/>"#,
        "maximize" => r#"<rect x="6" y="6" width="12" height="12" rx="1.4"/>"#,
        "restore" => {
            r#"<rect x="8" y="5" width="10" height="10" rx="1.2"/><path d="M6 9.5V17.5A1.5 1.5 0 0 0 7.5 19H15"/>"#
        }
        "close" => r#"<path d="M7 7l10 10M17 7 7 17"/>"#,
        _ => {
            r#"<rect x="4" y="4" width="6.5" height="6.5" rx="1.4"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1.4"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1.4"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.4"/>"#
        }
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="{stroke}" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
    )
}

fn squircle_tone(glyph: &str) -> (Color, &'static str) {
    match glyph {
        "files" => (Color::rgba(20, 150, 106, 46), "#d9ffe9"),
        "settings" => (Color::rgba(226, 164, 92, 41), "#ffe6c2"),
        "browser" => (Color::rgba(130, 170, 200, 41), "#e5f3ff"),
        _ => (Color::rgba(244, 241, 234, 20), "#f4f1ea"),
    }
}

fn icon_button(
    glyph: &str,
    name: impl Into<String>,
    code: u8,
    background: &str,
    size: f32,
) -> Component {
    glyph_face(
        glyph,
        name,
        code,
        Color::from_hex(background),
        "white",
        size,
        12.0,
        false,
    )
}

fn dock_icon(
    glyph: &str,
    name: impl Into<String>,
    code: u8,
    fill: Color,
    active: bool,
) -> Component {
    // Fine-pointer tray icons stay 40px. Launcher and keyboard reach the same
    // actions. Coarse pointers use 48px.
    let size = if state::pointer_is_coarse() {
        48.0
    } else {
        40.0
    };
    glyph_face(glyph, name, code, fill, "#f4f1ea", size, 12.0, active)
}

fn glyph_face(
    glyph: &str,
    name: impl Into<String>,
    code: u8,
    fill: Color,
    stroke: &str,
    size: f32,
    radius: f32,
    active: bool,
) -> Component {
    let glyph_px = (size * 0.5).round().clamp(16.0, 28.0);
    let mut children = vec![Component::svg_document(
        glyph_svg(glyph, stroke),
        24,
        24,
        Style {
            width: Dimension::Px(glyph_px),
            height: Dimension::Px(glyph_px),
            flex_shrink: 0.0,
            ..Style::default()
        },
    )];
    if active {
        children.push(Component::column(
            Style {
                position: Position::Absolute,
                left: Dimension::Px((size - 12.0) / 2.0),
                top: Dimension::Px(size - 6.0),
                width: Dimension::Px(12.0),
                height: Dimension::Px(3.0),
                background: Color::from_hex("#14966a"),
                border_radius: 99.0,
                ..Style::default()
            },
            vec![],
        ));
    }
    let mut button = Component::column(
        Style {
            position: Position::Relative,
            width: Dimension::Px(size),
            height: Dimension::Px(size),
            min_width: Dimension::Px(size),
            min_height: Dimension::Px(size),
            flex_shrink: 0.0,
            background: fill,
            border_radius: radius,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            // The name stays on the button for accessibility. Icon buttons must
            // not paint it: a CJK label is wider than the 40px face and draws
            // across the neighboring dock icons.
            color: Color::TRANSPARENT,
            overflow: Overflow::Hidden,
            ..Style::default()
        },
        children,
    );
    button.kind = ComponentKind::Button { label: name.into() };
    button.on_click = invoke(code);
    button
}

fn live_clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// One fixed cell per character so digits stay evenly spaced and the colon
/// does not pick up proportional side bearings.
fn tabular_clock(font_size: f32, weight: u16, color: Color) -> Component {
    let digit_w = (font_size * 0.62).round();
    let colon_w = (font_size * 0.36).round();
    let cells = live_clock()
        .chars()
        .map(|ch| {
            let width = if ch == ':' { colon_w } else { digit_w };
            Component::text(
                ch.to_string(),
                Style {
                    width: Dimension::Px(width),
                    min_width: Dimension::Px(width),
                    font_size,
                    font_weight: weight,
                    font_kerning: false,
                    text_align: TextAlign::Center,
                    color,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
            )
        })
        .collect();
    Component::row(
        Style {
            gap: 0.0,
            align_items: AlignItems::Center,
            flex_shrink: 0.0,
            ..Style::default()
        },
        cells,
    )
}

fn note_preedit(style: &mut Style, role: &str) {
    let Some(suffix) = ime_bar::preedit_suffix(role) else {
        return;
    };
    let mut props = style.custom_properties.take().unwrap_or_default();
    props.insert("--w3cos-ime-preedit".to_string(), suffix);
    style.custom_properties = Some(props);
}

fn set_receipt_filter(kind: i64) {
    state::set_signal(RECEIPT_FILTER_SIGNAL, kind);
    state::set_signal(RECEIPT_PAGE_SIGNAL, 0);
}

fn send_handoff() {
    let clipboard = if handoff::clipboard_sync_enabled() {
        read_clipboard()
    } else {
        None
    };
    let sealed = handoff::begin_send(
        receipt_entries(),
        &state::get_signal_text(DRAFT_SIGNAL),
        &state::get_signal_text(FILE_PREVIEW_SIGNAL),
        false,
        clipboard.as_deref(),
    );
    if sealed.is_none() {
        push_toast(&locale::t("handoff-no-peer"));
    }
}

fn handoff_notice_text(id: &str) -> String {
    match id {
        "handoff-no-answer" => locale::t_args(
            "handoff-no-answer",
            &[
                ("peer", &handoff::handoff_target_name()),
                ("here", &handoff::local_name()),
            ],
        ),
        "handoff-incoming" => {
            let from = handoff::incoming()
                .map(|(name, _, _)| name)
                .unwrap_or_default();
            locale::t_args("handoff-incoming", &[("name", &from)])
        }
        other => locale::t(other),
    }
}

fn clip_clock(unix: u64) -> String {
    let day = unix % 86_400;
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

fn clip_sheet_inside_incoming() -> bool {
    handoff::incoming().is_some() && (handoff::incoming_revealed() || posture() != Posture::Phone)
}

fn read_clipboard() -> Option<String> {
    let output = std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn copy_subject() {
    let subject = current_subject();
    COPIED_SUBJECT.with(|slot| *slot.borrow_mut() = subject.clone());
    if let Ok(mut child) = std::process::Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        && let Some(mut stdin) = child.stdin.take()
    {
        use std::io::Write;
        let _ = stdin.write_all(subject.as_bytes());
    }
    push_toast(&locale::t("receipt-copied"));
}

fn visible_receipts() -> Vec<String> {
    let filter = state::get_signal(RECEIPT_FILTER_SIGNAL);
    let query = state::get_signal_text(RECEIPT_QUERY_SIGNAL);
    let needle = query.trim().to_lowercase();
    receipt_entries()
        .into_iter()
        .filter(|line| {
            if !gesture::receipt_matches(filter, line) {
                return false;
            }
            if needle.is_empty() {
                return true;
            }
            let parts = parse_receipt(line);
            let hay = format!(
                "{} {} {} {}",
                parts.command, parts.utterance, parts.detail, parts.tier
            )
            .to_lowercase();
            hay.contains(&needle)
        })
        .collect()
}

fn live_battery() -> Option<String> {
    let raw = std::fs::read_to_string("/sys/class/power_supply/BAT0/capacity").ok()?;
    let pct = raw.trim().parse::<u32>().ok()?;
    if pct > 100 {
        return None;
    }
    Some(format!("{pct}%"))
}

fn tray_bell() -> Component {
    let icon = dock_icon(
        "bell",
        locale::t("tray-notify"),
        OP_NOTIFY_TOGGLE,
        Color::TRANSPARENT,
        state::get_signal(NOTIFY_SIGNAL) != 0,
    );
    let unread = state::get_signal(NOTIFY_UNREAD_SIGNAL);
    if unread <= 0 {
        return icon;
    }
    let size = if state::pointer_is_coarse() {
        48.0
    } else {
        40.0
    };
    let count = if unread > 9 {
        "9+".to_string()
    } else {
        unread.to_string()
    };
    Component::column(
        Style {
            position: Position::Relative,
            width: Dimension::Px(size),
            height: Dimension::Px(size),
            flex_shrink: 0.0,
            overflow: Overflow::Visible,
            ..Style::default()
        },
        vec![
            icon,
            Component::text(
                count,
                Style {
                    position: Position::Absolute,
                    right: Dimension::Px(-2.0),
                    top: Dimension::Px(-2.0),
                    min_width: Dimension::Px(16.0),
                    height: Dimension::Px(16.0),
                    padding: Edges::xy(3.0, 0.0),
                    border_radius: 99.0,
                    background: Color::from_hex("#9a3412"),
                    color: Color::WHITE,
                    font_size: 10.0,
                    font_weight: 700,
                    text_align: TextAlign::Center,
                    z_index: 2,
                    ..Style::default()
                },
            ),
        ],
    )
}

fn l2_badge() -> String {
    if session_l2_on() {
        locale::t("l2-badge-on")
    } else {
        locale::t("l2-badge-off")
    }
}

/// Tray label for local planning. It stays on the desktop taskbar, the phone
/// status row, and the tablet rail, so the off state is visible without
/// opening the task center. The button opens the planning settings.
fn l2_status_button(height: f32) -> Component {
    Component::button_with_click(
        l2_badge(),
        Style {
            background: Color::TRANSPARENT,
            color: if session_l2_on() {
                Color::from_hex("#b7c0ff")
            } else {
                muted_token()
            },
            font_size: 12.0,
            font_weight: 650,
            padding: Edges::xy(6.0, 0.0),
            height: Dimension::Px(height),
            min_height: Dimension::Px(height),
            border_radius: 8.0,
            ..Style::default()
        },
        invoke(OP_TRAY_L2),
    )
}

fn truncate_middle(value: &str, keep: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= keep {
        return value.to_string();
    }
    let head = keep / 2;
    let tail = keep - head - 1;
    let start: String = chars.iter().take(head).collect();
    let end: String = chars.iter().skip(chars.len() - tail).collect();
    format!("{start}…{end}")
}

fn current_subject() -> String {
    let owned = state::get_signal_text(SUBJECT_SIGNAL);
    if owned.is_empty() {
        LocalIdentityStub::SUBJECT.to_string()
    } else {
        owned
    }
}

fn process_command() -> String {
    let line = state::get_signal_text(TERMINAL_SIGNAL);
    line.split_whitespace()
        .next()
        .filter(|token| !token.is_empty())
        .unwrap_or("echo")
        .to_string()
}

const RECEIPT_PAGE_SIZE: usize = 6;

fn receipt_entries() -> Vec<String> {
    let log = state::get_signal_text(RECEIPT_SIGNAL);
    if log.is_empty() || log == "No receipts yet." {
        Vec::new()
    } else {
        log.lines().map(str::to_string).collect()
    }
}

fn receipt_page_count(len: usize) -> i64 {
    let pages = (len as i64 + RECEIPT_PAGE_SIZE as i64 - 1) / RECEIPT_PAGE_SIZE as i64;
    pages.max(1)
}

struct ReceiptParts {
    id: String,
    decision: String,
    command: String,
    tier: String,
    actor: String,
    detail: String,
    recorded_at: String,
    utterance: String,
    signed: bool,
}

fn parse_receipt(line: &str) -> ReceiptParts {
    let (head, rest) = line.split_once(" — ").unwrap_or((line, ""));
    let (detail, utterance) = rest.split_once(" @@ ").unwrap_or((rest, ""));
    let mut tokens = head.split_whitespace();
    let id = tokens.next().unwrap_or("#").to_string();
    let decision = tokens.next().unwrap_or("").to_string();
    let command = tokens.next().unwrap_or("").to_string();
    let mut tier = String::new();
    let mut actor = String::new();
    let mut recorded_at = String::new();
    let mut signed = false;
    for token in tokens {
        if let Some(value) = token.strip_prefix("tier=") {
            tier = value.to_string();
        } else if let Some(value) = token.strip_prefix("actor=") {
            actor = value.to_string();
        } else if let Some(value) = token.strip_prefix("ts=") {
            recorded_at = value.to_string();
        } else if let Some(value) = token.strip_prefix("signed=") {
            signed = value == "1";
        }
    }
    ReceiptParts {
        id,
        decision,
        command,
        tier,
        actor,
        detail: detail.to_string(),
        recorded_at,
        utterance: utterance.to_string(),
        signed,
    }
}

fn format_receipt_clock(recorded_at: &str) -> String {
    let Ok(millis) = recorded_at.parse::<i64>() else {
        return String::new();
    };
    if millis <= 0 {
        return String::new();
    }
    let Some(stamp) = chrono::DateTime::from_timestamp_millis(millis) else {
        return String::new();
    };
    stamp
        .with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string()
}

fn decision_label(decision: &str) -> String {
    match decision {
        "allowed" => locale::t("decision-allowed"),
        "denied" => locale::t("decision-denied"),
        "cancelled" => locale::t("decision-cancelled"),
        "unrecognized" => locale::t("decision-unrecognized"),
        other => other.to_string(),
    }
}

fn decision_colors(decision: &str) -> (Color, Color) {
    match decision {
        "allowed" => (Color::from_hex("#0c6d4c"), Color::from_hex("#e9fff5")),
        "denied" => (Color::from_hex("#9a3412"), Color::from_hex("#ffe8e4")),
        "cancelled" => (Color::from_hex("#b45309"), Color::from_hex("#fff4d6")),
        "unrecognized" => (Color::from_hex("#1d4e89"), Color::from_hex("#e7f1ff")),
        _ => (Color::from_hex("#31475e"), Color::from_hex("#f4f7fb")),
    }
}

fn layer_label(tier: &str) -> String {
    match tier {
        "L0" => locale::t("layer-l0"),
        "L1" => locale::t("layer-l1"),
        "L2" => locale::t("layer-l2"),
        "" => String::new(),
        other => other.to_string(),
    }
}

fn present_receipt(line: &str) -> String {
    let parts = parse_receipt(line);
    let word = decision_label(&parts.decision);
    if parts.detail.is_empty() {
        format!("{} {word}", parts.id)
    } else {
        format!("{} {word} {}", parts.id, parts.detail)
    }
}

fn build_intent_panel() -> Component {
    let go = touch_min();
    let mut input_style = Style {
        flex_grow: 1.0,
        font_size: 16.0,
        color: ink_token(),
        background: Color::TRANSPARENT,
        padding: Edges::ZERO,
        min_height: Dimension::Px(go),
        ..Style::default()
    };
    input_style.custom_properties = Some(HashMap::from([
        ("--w3cos-intent-submit".to_string(), OP_PROPOSE.to_string()),
        ("--w3cos-ime".to_string(), "intent".to_string()),
    ]));
    note_preedit(&mut input_style, "intent");
    let mut go_button = glyph_face(
        "arrow",
        locale::t("propose"),
        OP_PROPOSE,
        Color::from_hex("#0c6d4c"),
        "#e9fff5",
        go,
        10.0,
        false,
    );
    go_button.on_click = EventAction::Invoke {
        code: OP_PROPOSE,
        input_placeholder: locale::t("intent-placeholder"),
    };
    let mut rows = vec![
        Component::text(
            locale::t("intent-hint"),
            Style {
                font_size: 12.0,
                color: muted_token(),
                text_align: TextAlign::Center,
                text_shadows: label_shadow(),
                ..Style::default()
            },
        ),
        Component::row(
            Style {
                height: Dimension::Px(if state::pointer_is_coarse() {
                    56.0
                } else {
                    52.0
                }),
                padding: Edges {
                    top: Spacing::Px(0.0),
                    right: Spacing::Px(8.0),
                    bottom: Spacing::Px(0.0),
                    left: Spacing::Px(16.0),
                },
                gap: 8.0,
                align_items: AlignItems::Center,
                background: Color::rgba(16, 22, 29, 210),
                border_width: 1.0,
                border_color: line_strong(),
                border_radius: 16.0,
                box_shadow: Some(shadow_sm()),
                backdrop_blur: Some(16.0),
                ..Style::default()
            },
            vec![
                Component::text_input(
                    state::get_signal_text(DRAFT_SIGNAL),
                    locale::t("intent-placeholder"),
                    input_style,
                ),
                go_button,
            ],
        ),
    ];
    if state::get_signal(PHASE_SIGNAL) == 3 && session_l2_on() {
        rows.push(Component::text(
            locale::t("planning"),
            Style {
                font_size: 13.0,
                color: Color::from_hex("#b7c0ff"),
                text_align: TextAlign::Center,
                ..Style::default()
            },
        ));
    }
    Component::column(
        Style {
            flex_shrink: 0.0,
            gap: 8.0,
            background: Color::TRANSPARENT,
            ..Style::default()
        },
        rows,
    )
}

fn floating_intent() -> Component {
    let (width, height) = state::shell_viewport();
    let bar_w = 560.0_f32.min((width - 32.0).max(280.0));
    let mut bar = build_intent_panel();
    bar.style.position = Position::Absolute;
    bar.style.width = Dimension::Px(bar_w);
    bar.style.left = Dimension::Px(((width - bar_w) / 2.0).max(8.0));
    bar.style.top = Dimension::Px(intent_block_top(height));
    bar.style.z_index = CHROME_Z;
    bar
}

fn app_button_name(id: i64, focused: bool) -> String {
    let name = app_label(id);
    if focused {
        format!("{name} · {}", locale::t("using-now"))
    } else {
        name
    }
}

fn taskbar_app(id: i64, current: i64) -> Component {
    let focused = current == id;
    let glyph = RESIDENT_APPS
        .iter()
        .find(|app| app.id == id)
        .map(|app| app.glyph)
        .unwrap_or("grid");
    let (fill, _) = squircle_tone(glyph);
    dock_icon(
        glyph,
        app_button_name(id, focused),
        focus_code(id),
        fill,
        focused,
    )
}

fn build_floating_taskbar() -> Component {
    let current = state::get_signal(APP_SIGNAL);
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut leading = vec![dock_icon(
        "grid",
        locale::t("launcher-all"),
        OP_LAUNCHER,
        Color::TRANSPARENT,
        false,
    )];
    for app in RESIDENT_APPS {
        if open & app_bit(app.id) != 0 {
            leading.push(taskbar_app(app.id, current));
        }
    }
    let mut tray = vec![
        dock_icon(
            "tasks",
            locale::t("tasks"),
            OP_TOGGLE_TASKS,
            Color::TRANSPARENT,
            state::get_signal(TASKS_SIGNAL) != 0,
        ),
        dock_icon(
            if unlocked { "lock-open" } else { "lock" },
            if unlocked {
                locale::t("unlocked")
            } else {
                locale::t("locked")
            },
            OP_LOCK,
            Color::TRANSPARENT,
            false,
        ),
        dock_icon(
            "devices",
            locale::t("settings-devices"),
            OP_SETTINGS_DEVICES,
            Color::TRANSPARENT,
            false,
        ),
        tray_bell(),
        l2_status_button(touch_min()),
    ];
    if let Some(battery) = live_battery() {
        tray.push(Component::text(
            battery,
            Style {
                font_size: 13.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        ));
    }
    tray.push(tabular_clock(13.0, 600, ink_token()));
    let bar_h = if state::pointer_is_coarse() {
        64.0
    } else {
        56.0
    };
    Component::row(
        Style {
            height: Dimension::Px(bar_h),
            padding: Edges::xy(10.0, 0.0),
            gap: 8.0,
            align_items: AlignItems::Center,
            background: Color::rgba(12, 18, 24, 190),
            border_width: 1.0,
            border_color: Color::rgba(244, 241, 234, 31),
            border_radius: 16.0,
            box_shadow: Some(shadow_sm()),
            backdrop_blur: Some(18.0),
            box_sizing: BoxSizing::BorderBox,
            ..Style::default()
        },
        vec![
            Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 1.0,
                    ..Style::default()
                },
                leading,
            ),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    min_width: Dimension::Px(8.0),
                    ..Style::default()
                },
                vec![],
            ),
            Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
                tray,
            ),
        ],
    )
}

fn floating_taskbar() -> Component {
    let (width, height) = state::shell_viewport();
    let mut bar = build_floating_taskbar();
    bar.style.position = Position::Absolute;
    bar.style.left = Dimension::Px(16.0);
    let bar_h = if state::pointer_is_coarse() {
        64.0
    } else {
        56.0
    };
    bar.style.top = Dimension::Px((height - 12.0 - bar_h).max(8.0));
    bar.style.width = Dimension::Px((width - 32.0).max(280.0));
    bar.style.height = Dimension::Px(bar_h);
    bar.style.z_index = CHROME_Z;
    bar
}

fn build_phone_home() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut status_right = vec![
        // The status row is 36px tall, so these glyphs stay 28px.
        // Home tiles are 64px and sheet actions use the coarse 48px target.
        l2_status_button(28.0),
        glyph_face(
            "tasks",
            locale::t("tasks"),
            OP_TOGGLE_TASKS,
            Color::TRANSPARENT,
            "#f4f1ea",
            28.0,
            8.0,
            state::get_signal(TASKS_SIGNAL) != 0,
        ),
        glyph_face(
            if unlocked { "lock-open" } else { "lock" },
            if unlocked {
                locale::t("unlocked")
            } else {
                locale::t("locked")
            },
            OP_LOCK,
            Color::TRANSPARENT,
            "#f4f1ea",
            28.0,
            8.0,
            false,
        ),
    ];
    if let Some(battery) = live_battery() {
        status_right.push(Component::text(
            battery,
            Style {
                font_size: 12.0,
                color: ink_token(),
                ..Style::default()
            },
        ));
    }
    let mut children = vec![
        Component::row(
            Style {
                height: Dimension::Px(36.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: Edges::xy(6.0, 0.0),
                ..Style::default()
            },
            vec![
                tabular_clock(14.0, 650, ink_token()),
                Component::row(
                    Style {
                        gap: 6.0,
                        align_items: AlignItems::Center,
                        ..Style::default()
                    },
                    status_right,
                ),
            ],
        ),
        build_desktop_icons(),
    ];
    if std::env::var_os("W3COS_SHARED_LAUNCHER").is_some() {
        children.push(build_shared_launcher());
    }
    Component::column(
        Style {
            flex_grow: 1.0,
            min_height: Dimension::Px(0.0),
            overflow: Overflow::Auto,
            padding: Edges {
                top: Spacing::Px(16.0),
                right: Spacing::Px(16.0),
                bottom: Spacing::Px(8.0),
                left: Spacing::Px(16.0),
            },
            gap: 8.0,
            background: Color::from_hex("#121b24"),
            background_image: Some(WALLPAPER.to_string()),
            ..Style::default()
        },
        children,
    )
}

fn showing_resident_app() -> Option<i64> {
    let current = state::get_signal(APP_SIGNAL);
    if !resident(current) {
        return None;
    }
    let bit = app_bit(current);
    let open = state::get_signal(OPEN_MASK_SIGNAL) & bit != 0;
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL) & bit != 0;
    if open && !minimized {
        Some(current)
    } else {
        None
    }
}

fn build_phone_stack() -> Component {
    let mut children = Vec::new();
    if let Some(current) = showing_resident_app() {
        children.push(Component::row(
            Style {
                flex_shrink: 0.0,
                gap: 8.0,
                padding: Edges::xy(12.0, 10.0),
                align_items: AlignItems::Center,
                background: Color::from_hex("#162433"),
                ..Style::default()
            },
            vec![
                chrome_button(locale::t("back"), OP_FOCUS_DESKTOP, "#3d5570"),
                Component::text(
                    app_label(current),
                    Style {
                        font_size: 16.0,
                        font_weight: 700,
                        color: Color::from_hex("#f4f7fb"),
                        ..Style::default()
                    },
                ),
            ],
        ));
        let mut body = app_body(current);
        body.style.flex_grow = 1.0;
        body.style.min_height = Dimension::Px(0.0);
        children.push(body);
    } else {
        children.push(build_phone_home());
    }
    children.push(Component::column(
        Style {
            flex_shrink: 0.0,
            padding: Edges::xy(12.0, 0.0),
            ..Style::default()
        },
        vec![build_intent_panel()],
    ));
    if let Some(bar) = ime_bar::phone_smart_bar() {
        children.push(bar);
    }
    if let Some(strip) = ime_bar::phone_candidate_strip() {
        children.push(strip);
    }
    children.push(Component::row(
        Style {
            height: Dimension::Px(20.0),
            flex_shrink: 0.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..Style::default()
        },
        vec![Component::column(
            Style {
                width: Dimension::Px(128.0),
                height: Dimension::Px(5.0),
                border_radius: 99.0,
                background: Color::rgba(244, 241, 234, 140),
                ..Style::default()
            },
            vec![],
        )],
    ));
    if ime_bar::keyboard_open() {
        children.push(Component::column(
            Style {
                height: Dimension::Px(ime_bar::KEYBOARD_HEIGHT),
                flex_shrink: 0.0,
                ..Style::default()
            },
            vec![],
        ));
    }
    Component::column(
        Style {
            flex_grow: 1.0,
            height: Dimension::Vh(100.0),
            min_height: Dimension::Px(0.0),
            ..Style::default()
        },
        children,
    )
}

fn build_tablet_rail() -> Component {
    let current = state::get_signal(APP_SIGNAL);
    let mut icons = vec![icon_button(
        "grid",
        String::new(),
        OP_LAUNCHER,
        "#1c8f62",
        48.0,
    )];
    for app in RESIDENT_APPS {
        let focused = current == app.id;
        let mut button = icon_button(
            app.glyph,
            app_button_name(app.id, focused),
            focus_code(app.id),
            app.color,
            48.0,
        );
        if focused {
            button.style.border_width = 2.0;
            button.style.border_color = Color::from_hex("#f4f7fb");
        }
        icons.push(button);
    }
    icons.push(icon_button(
        "tasks",
        locale::t("tasks"),
        OP_TOGGLE_TASKS,
        "#31475e",
        48.0,
    ));
    icons.push(l2_status_button(48.0));
    Component::column(
        Style {
            width: Dimension::Px(72.0),
            flex_shrink: 0.0,
            padding: Edges::xy(8.0, 12.0),
            gap: 10.0,
            align_items: AlignItems::Center,
            background: Color::from_hex("#162433"),
            ..Style::default()
        },
        icons,
    )
}

fn build_tablet_content() -> Component {
    if let Some(current) = showing_resident_app() {
        let mut body = app_body(current);
        body.style.flex_grow = 1.0;
        body.style.min_width = Dimension::Px(0.0);
        body.style.min_height = Dimension::Px(0.0);
        body
    } else {
        build_desktop()
    }
}

fn build_tablet_row() -> Component {
    let (width, _) = state::shell_viewport();
    let bar_w = 520.0_f32.min((width - 32.0).max(280.0));
    let mut intent = build_intent_panel();
    intent.style.width = Dimension::Px(bar_w);
    Component::column(
        Style {
            flex_grow: 1.0,
            height: Dimension::Vh(100.0),
            min_height: Dimension::Px(0.0),
            ..Style::default()
        },
        vec![
            Component::row(
                Style {
                    flex_grow: 1.0,
                    min_height: Dimension::Px(0.0),
                    min_width: Dimension::Px(0.0),
                    ..Style::default()
                },
                vec![build_tablet_rail(), build_tablet_content()],
            ),
            Component::row(
                Style {
                    flex_shrink: 0.0,
                    justify_content: JustifyContent::Center,
                    padding: Edges::xy(12.0, 10.0),
                    ..Style::default()
                },
                vec![intent],
            ),
        ],
    )
}

fn dialog_shell(children: Vec<Component>) -> Component {
    let phone = posture() == Posture::Phone;
    Component::column(
        Style {
            gap: 10.0,
            padding: Edges::all(if phone { 16.0 } else { 20.0 }),
            background: if phone {
                Color::from_hex("#1c2631")
            } else {
                sheet_fill()
            },
            border_width: if phone { 0.0 } else { 1.0 },
            border_color: line_strong(),
            border_radius: if phone { 22.0 } else { 16.0 },
            box_shadow: Some(shadow_lg()),
            box_shadows: vec![inset_highlight(15)],
            ..Style::default()
        },
        children,
    )
}

fn sheet_button(label: impl Into<String>, code: u8, kind: SheetKind) -> Component {
    let mut button = action_button(label, code, kind);
    if posture() != Posture::Desktop {
        button.style.height = Dimension::Px(48.0);
        button.style.min_height = Dimension::Px(48.0);
        button.style.flex_grow = 1.0;
    }
    button
}

fn layer_mark(tier: &str) -> Option<Component> {
    // Words come first. Color repeats the tier; it is not the only signal.
    // L0 uses a solid pill so the rules badge is visible on the card.
    let (key, bg, fg) = match tier {
        "L2" => (
            "layer-l2",
            Color::from_hex("#2a315c"),
            Color::from_hex("#b7c0ff"),
        ),
        "L1" => (
            "layer-l1",
            Color::from_hex("#f0d7b4"),
            Color::from_hex("#2a1c0c"),
        ),
        "L0" => ("layer-l0", Color::from_hex("#243444"), ink_token()),
        _ => return None,
    };
    Some(Component::text(
        locale::t(key),
        Style {
            font_size: 12.0,
            font_weight: 650,
            color: fg,
            background: bg,
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 999.0,
            padding: Edges::xy(8.0, 2.0),
            align_self: AlignSelf::FlexStart,
            ..Style::default()
        },
    ))
}

fn mark_initial_focus(button: &mut Component) {
    let props = button
        .style
        .custom_properties
        .get_or_insert_with(HashMap::new);
    props.insert("--w3cos-initial-focus".to_string(), "1".to_string());
}

fn focus_kind(default: &str) -> String {
    let set = state::get_signal_text(CONFIRM_FOCUS_SIGNAL);
    if set.is_empty() {
        default.to_string()
    } else {
        set
    }
}

fn build_locale_confirm() -> Component {
    let tag = state::get_signal_text(PENDING_LOCALE_SIGNAL);
    let name = locale_self_name(&tag);
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut rows = Vec::new();
    if let Some(mark) = layer_mark("L0") {
        rows.push(mark);
    }
    rows.push(Component::text(
        locale::t_args("locale-confirm-title", &[("name", &name)]),
        Style {
            font_size: 22.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    ));
    rows.push(Component::text(
        locale::t_args("locale-confirm-impact", &[("name", &name)]),
        Style {
            font_size: 14.0,
            color: muted_ink(),
            ..Style::default()
        },
    ));
    let mut cancel = sheet_button(locale::t("cancel"), OP_CANCEL, SheetKind::Ghost);
    let accept = if unlocked {
        sheet_button(locale::t("confirm"), OP_CONFIRM, SheetKind::Primary)
    } else {
        sheet_button(locale::t("lock-first"), OP_UNLOCK, SheetKind::Primary)
    };
    mark_initial_focus(&mut cancel);
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![cancel, accept],
    ));
    rows.push(Component::text(
        locale::t("confirm-footnote"),
        Style {
            font_size: 12.0,
            color: faint_token(),
            ..Style::default()
        },
    ));
    dialog_shell(rows)
}

fn build_confirm_dialog() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let tier = state::get_signal_text(TIER_SIGNAL);
    let focus = focus_kind("confirm");
    let mut rows = Vec::new();
    if let Some(mark) = layer_mark(&tier) {
        rows.push(mark);
    }
    rows.push(Component::text(
        state::get_signal_text(SUMMARY_SIGNAL),
        Style {
            font_size: 22.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    ));
    if tier == "L1" {
        rows.push(Component::text(
            locale::t("l1-same"),
            Style {
                font_size: 13.0,
                color: Color::from_hex("#f0d7b4"),
                ..Style::default()
            },
        ));
    }
    rows.push(Component::text(
        state::get_signal_text(IMPACT_SIGNAL),
        Style {
            font_size: 14.0,
            color: muted_ink(),
            ..Style::default()
        },
    ));
    let mut cancel = sheet_button(locale::t("cancel"), OP_CANCEL, SheetKind::Ghost);
    let mut accept = if unlocked {
        sheet_button(locale::t("confirm"), OP_CONFIRM, SheetKind::Primary)
    } else {
        sheet_button(locale::t("lock-first"), OP_UNLOCK, SheetKind::Primary)
    };
    if focus == "cancel" {
        mark_initial_focus(&mut cancel);
    } else if unlocked {
        mark_initial_focus(&mut accept);
    }
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![cancel, accept],
    ));
    rows.push(Component::text(
        locale::t("confirm-footnote"),
        Style {
            font_size: 12.0,
            color: faint_token(),
            ..Style::default()
        },
    ));
    let mut card = dialog_shell(rows);
    mark_dialog(&mut card, "confirm");
    card
}

fn disabled_choice(label: impl Into<String>) -> Component {
    let mut button = sheet_button(label, OP_PERM_DENY, SheetKind::Ghost);
    button.on_click = EventAction::None;
    button.style.opacity = 0.4;
    button
}

fn build_permission_sheet() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let command = process_command();
    let mut rows = vec![
        Component::text(
            locale::t("perm-cap-title"),
            Style {
                font_size: 12.0,
                font_weight: 700,
                color: Color::from_hex("#e2a45c"),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ),
        Component::text(
            locale::t_args("perm-sheet-title", &[("command", &command)]),
            Style {
                font_size: 22.0,
                font_weight: 700,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ),
    ];
    rows.push(Component::text(
        if unlocked {
            locale::t("perm-sheet-detail")
        } else {
            locale::t("perm-locked")
        },
        Style {
            font_size: 14.0,
            font_weight: if unlocked { 400 } else { 700 },
            color: if unlocked {
                Color::from_hex("#f0d7b4")
            } else {
                Color::from_hex("#ffe08a")
            },
            ..Style::default()
        },
    ));
    rows.push(Component::text(
        locale::t("perm-sheet-scope"),
        Style {
            font_size: 13.0,
            color: Color::from_hex("#fff4e5"),
            background: Color::rgba(226, 164, 92, 28),
            border_radius: 12.0,
            padding: Edges::all(12.0),
            ..Style::default()
        },
    ));
    let mut deny = if unlocked {
        sheet_button(locale::t("perm-deny"), OP_PERM_DENY, SheetKind::Danger)
    } else {
        disabled_choice(locale::t("perm-deny"))
    };
    let once = if unlocked {
        sheet_button(locale::t("perm-once"), OP_PERM_ONCE, SheetKind::Warm)
    } else {
        disabled_choice(locale::t("perm-once"))
    };
    if unlocked && focus_kind("deny") == "deny" {
        mark_initial_focus(&mut deny);
    }
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![deny, once],
    ));
    rows.push(Component::text(
        locale::t("perm-footnote"),
        Style {
            font_size: 12.0,
            color: Color::from_hex("#e2a45c"),
            ..Style::default()
        },
    ));
    let mut card = permission_shell(rows);
    mark_dialog(&mut card, "permission");
    card
}

fn sandbox_scope_key() -> &'static str {
    match (
        state::get_signal(SANDBOX_FS_SIGNAL) != 0,
        state::get_signal(SANDBOX_NET_SIGNAL) != 0,
    ) {
        (false, false) => "sandbox-scope-none",
        (true, false) => "sandbox-scope-fs",
        (false, true) => "sandbox-scope-net",
        (true, true) => "sandbox-scope-both",
    }
}

fn build_sandbox_sheet() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let command = sandbox_command_label();
    let mut rows = vec![
        Component::text(
            locale::t("sandbox-cap-title"),
            Style {
                font_size: 12.0,
                font_weight: 700,
                color: Color::from_hex("#e2a45c"),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ),
        Component::text(
            locale::t_args("sandbox-sheet-title", &[("command", &command)]),
            Style {
                font_size: 22.0,
                font_weight: 700,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ),
    ];
    rows.push(Component::text(
        if unlocked {
            locale::t("sandbox-sheet-detail")
        } else {
            locale::t("sandbox-locked")
        },
        Style {
            font_size: 14.0,
            font_weight: if unlocked { 400 } else { 700 },
            color: if unlocked {
                Color::from_hex("#f0d7b4")
            } else {
                Color::from_hex("#ffe08a")
            },
            ..Style::default()
        },
    ));
    rows.push(Component::text(
        locale::t(sandbox_scope_key()),
        Style {
            font_size: 13.0,
            color: Color::from_hex("#fff4e5"),
            background: Color::rgba(226, 164, 92, 28),
            border_radius: 12.0,
            padding: Edges::all(12.0),
            ..Style::default()
        },
    ));
    let fs_key = if state::get_signal(SANDBOX_FS_SIGNAL) != 0 {
        "sandbox-fs-on"
    } else {
        "sandbox-fs-off"
    };
    let net_key = if state::get_signal(SANDBOX_NET_SIGNAL) != 0 {
        "sandbox-net-on"
    } else {
        "sandbox-net-off"
    };
    let fs = if unlocked {
        sheet_button(locale::t(fs_key), OP_SANDBOX_FS, SheetKind::Ghost)
    } else {
        disabled_choice(locale::t(fs_key))
    };
    let net = if unlocked {
        sheet_button(locale::t(net_key), OP_SANDBOX_NET, SheetKind::Ghost)
    } else {
        disabled_choice(locale::t(net_key))
    };
    rows.push(Component::row(
        Style {
            gap: 8.0,
            ..Style::default()
        },
        vec![fs, net],
    ));
    let mut deny = if unlocked {
        sheet_button(locale::t("sandbox-deny"), OP_PERM_DENY, SheetKind::Danger)
    } else {
        disabled_choice(locale::t("sandbox-deny"))
    };
    let once = if unlocked {
        sheet_button(locale::t("sandbox-once"), OP_PERM_ONCE, SheetKind::Warm)
    } else {
        disabled_choice(locale::t("sandbox-once"))
    };
    if unlocked {
        mark_initial_focus(&mut deny);
    }
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![deny, once],
    ));
    rows.push(Component::text(
        locale::t("sandbox-footnote"),
        Style {
            font_size: 12.0,
            color: Color::from_hex("#e2a45c"),
            ..Style::default()
        },
    ));
    let mut card = permission_shell(rows);
    mark_dialog(&mut card, "sandbox");
    card
}

fn build_chain_sheet() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let endpoint = state::get_signal_text(CHAIN_RPC_SIGNAL);
    let origin = w3cos_ai_bridge::chain::parse_endpoint(&endpoint)
        .map(|parsed| parsed.origin)
        .unwrap_or_default();
    let mut rows = vec![
        Component::text(
            locale::t("chain-cap-title"),
            Style {
                font_size: 12.0,
                font_weight: 700,
                color: Color::from_hex("#e2a45c"),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ),
        Component::text(
            locale::t("chain-sheet-title"),
            Style {
                font_size: 22.0,
                font_weight: 700,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ),
        Component::text(
            state::get_signal_text(SUMMARY_SIGNAL),
            Style {
                font_size: 16.0,
                font_weight: 650,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ),
    ];
    rows.push(Component::text(
        if unlocked {
            locale::t("chain-sheet-detail")
        } else {
            locale::t("perm-locked")
        },
        Style {
            font_size: 14.0,
            color: Color::from_hex("#f0d7b4"),
            ..Style::default()
        },
    ));
    let scope = if origin.is_empty() {
        locale::t("chain-sheet-empty")
    } else {
        locale::t_args("chain-sheet-scope", &[("origin", &origin)])
    };
    rows.push(Component::text(
        scope,
        Style {
            font_size: 13.0,
            color: Color::from_hex("#fff4e5"),
            background: Color::rgba(226, 164, 92, 28),
            border_radius: 12.0,
            padding: Edges::all(12.0),
            ..Style::default()
        },
    ));
    rows.push(Component::text(
        state::get_signal_text(IMPACT_SIGNAL),
        Style {
            font_size: 13.0,
            color: Color::from_hex("#f0d7b4"),
            ..Style::default()
        },
    ));
    let mut deny = if unlocked {
        sheet_button(locale::t("chain-deny"), OP_CHAIN_DENY, SheetKind::Danger)
    } else {
        disabled_choice(locale::t("chain-deny"))
    };
    let once = if unlocked {
        sheet_button(locale::t("chain-once"), OP_CHAIN_ONCE, SheetKind::Warm)
    } else {
        disabled_choice(locale::t("chain-once"))
    };
    if unlocked {
        mark_initial_focus(&mut deny);
    }
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![deny, once],
    ));
    rows.push(Component::text(
        locale::t("chain-footnote"),
        Style {
            font_size: 12.0,
            color: Color::from_hex("#e2a45c"),
            ..Style::default()
        },
    ));
    let mut card = permission_shell(rows);
    mark_dialog(&mut card, "chain");
    card
}

fn build_sign_sheet() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let summary = state::get_signal_text(SIGN_SUMMARY_SIGNAL);
    let impact = state::get_signal_text(SIGN_IMPACT_SIGNAL);
    let message = state::get_signal_text(SIGN_MESSAGE_SIGNAL);
    let mut rows = vec![
        Component::text(
            locale::t("sign-cap-title"),
            Style {
                font_size: 12.0,
                font_weight: 700,
                color: Color::from_hex("#e2a45c"),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ),
        Component::text(
            locale::t("sign-sheet-title"),
            Style {
                font_size: 22.0,
                font_weight: 700,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ),
    ];
    if let Some(mark) = layer_mark("L2") {
        rows.push(mark);
    }
    rows.push(Component::text(
        if unlocked {
            locale::t("sign-sheet-detail")
        } else {
            locale::t("sign-locked")
        },
        Style {
            font_size: 14.0,
            font_weight: if unlocked { 400 } else { 700 },
            color: if unlocked {
                Color::from_hex("#f0d7b4")
            } else {
                Color::from_hex("#ffe08a")
            },
            ..Style::default()
        },
    ));
    if !summary.is_empty() {
        rows.push(Component::text(
            summary,
            Style {
                font_size: 15.0,
                font_weight: 650,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ));
    }
    rows.push(Component::text(
        if impact.is_empty() {
            locale::t("sign-sheet-scope")
        } else {
            impact
        },
        Style {
            font_size: 13.0,
            color: Color::from_hex("#fff4e5"),
            background: Color::rgba(226, 164, 92, 28),
            border_radius: 12.0,
            padding: Edges::all(12.0),
            ..Style::default()
        },
    ));
    rows.extend(message_rows(&message));
    let mut deny = if unlocked {
        sheet_button(locale::t("sign-deny"), OP_SIGN_DENY, SheetKind::Danger)
    } else {
        disabled_choice(locale::t("sign-deny"))
    };
    let once = if unlocked {
        sheet_button(locale::t("sign-once"), OP_SIGN_ONCE, SheetKind::Warm)
    } else {
        disabled_choice(locale::t("sign-once"))
    };
    if unlocked && focus_kind("deny") == "deny" {
        mark_initial_focus(&mut deny);
    }
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![deny, once],
    ));
    rows.push(Component::text(
        locale::t("sign-footnote"),
        Style {
            font_size: 12.0,
            color: Color::from_hex("#e2a45c"),
            ..Style::default()
        },
    ));
    let mut card = permission_shell(rows);
    mark_dialog(&mut card, "sign");
    card
}

fn message_rows(message: &str) -> Vec<Component> {
    message
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            Component::text(
                line.to_string(),
                Style {
                    font_size: 13.0,
                    color: Color::from_hex("#f0d7b4"),
                    font_family: Some("ui-monospace, monospace".to_string()),
                    ..Style::default()
                },
            )
        })
        .collect()
}

fn build_broadcast_sheet() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let endpoint = state::get_signal_text(CHAIN_RPC_SIGNAL);
    let origin = w3cos_ai_bridge::chain::parse_endpoint(&endpoint)
        .map(|parsed| parsed.origin)
        .unwrap_or_default();
    let mut rows = vec![
        Component::text(
            locale::t("broadcast-cap-title"),
            Style {
                font_size: 12.0,
                font_weight: 700,
                color: Color::from_hex("#e2a45c"),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ),
        Component::text(
            locale::t("broadcast-sheet-title"),
            Style {
                font_size: 22.0,
                font_weight: 700,
                color: Color::from_hex("#fff4e5"),
                ..Style::default()
            },
        ),
    ];
    if let Some(mark) = layer_mark("L2") {
        rows.push(mark);
    }
    rows.push(Component::text(
        if unlocked {
            locale::t("broadcast-sheet-detail")
        } else {
            locale::t("broadcast-locked")
        },
        Style {
            font_size: 14.0,
            color: Color::from_hex("#f0d7b4"),
            ..Style::default()
        },
    ));
    rows.extend(message_rows(&state::get_signal_text(SIGN_MESSAGE_SIGNAL)));
    let scope = if origin.is_empty() {
        locale::t("broadcast-sheet-empty")
    } else {
        locale::t_args("broadcast-sheet-scope", &[("origin", &origin)])
    };
    rows.push(Component::text(
        scope,
        Style {
            font_size: 13.0,
            color: Color::from_hex("#fff4e5"),
            background: Color::rgba(226, 164, 92, 28),
            border_radius: 12.0,
            padding: Edges::all(12.0),
            ..Style::default()
        },
    ));
    let mut deny = if unlocked {
        sheet_button(locale::t("broadcast-deny"), OP_TX_HOLD, SheetKind::Danger)
    } else {
        disabled_choice(locale::t("broadcast-deny"))
    };
    let once = if unlocked {
        sheet_button(locale::t("broadcast-once"), OP_TX_SEND, SheetKind::Warm)
    } else {
        disabled_choice(locale::t("broadcast-once"))
    };
    if unlocked {
        mark_initial_focus(&mut deny);
    }
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![deny, once],
    ));
    rows.push(Component::text(
        locale::t("broadcast-footnote"),
        Style {
            font_size: 12.0,
            color: Color::from_hex("#e2a45c"),
            ..Style::default()
        },
    ));
    let mut card = permission_shell(rows);
    mark_dialog(&mut card, "broadcast");
    card
}

fn permission_shell(children: Vec<Component>) -> Component {
    let phone = posture() == Posture::Phone;
    Component::column(
        Style {
            gap: 10.0,
            padding: Edges::all(if phone { 16.0 } else { 20.0 }),
            background: Color::from_hex("#241c14"),
            border_width: 1.0,
            border_color: Color::from_hex("#e2a45c"),
            border_radius: if phone { 22.0 } else { 16.0 },
            box_shadow: Some(shadow_lg()),
            ..Style::default()
        },
        children,
    )
}

fn build_lock_screen() -> Component {
    let subject = truncate_middle(&current_subject(), 28);
    let (width, _) = state::shell_viewport();
    let card_w = 420.0_f32.min((width - 32.0).max(280.0));
    let mut field_style = Style {
        width: Dimension::Px(card_w - 56.0),
        font_size: 15.0,
        color: Color::from_hex("#1a2836"),
        background: Color::from_hex("#f4f7fb"),
        border_radius: 10.0,
        padding: Edges::xy(12.0, 10.0),
        ..Style::default()
    };
    field_style.custom_properties = Some(HashMap::from([
        ("--w3cos-intent-submit".to_string(), OP_UNLOCK.to_string()),
        ("--w3cos-ime".to_string(), "secure".to_string()),
    ]));
    let passphrase = Component::secure_text_input(
        state::get_signal_text(LOCK_PASSPHRASE_SIGNAL),
        locale::t("lock-passphrase"),
        field_style,
    );
    let mut unlock = action_button(locale::t("lock-action"), OP_UNLOCK, SheetKind::Primary);
    unlock.on_click = EventAction::Invoke {
        code: OP_UNLOCK,
        input_placeholder: locale::t("lock-passphrase"),
    };
    unlock.style.width = Dimension::Px(card_w - 56.0);
    unlock.style.height = Dimension::Px(if state::pointer_is_coarse() {
        48.0
    } else {
        44.0
    });
    unlock.style.border_radius = 12.0;
    let keyboard_pad = if ime_bar::keyboard_open() {
        ime_bar::KEYBOARD_HEIGHT
    } else {
        0.0
    };
    Component::column(
        Style {
            width: Dimension::Vw(100.0),
            height: Dimension::Vh(100.0),
            background: Color::from_hex("#101820"),
            background_image: Some(WALLPAPER.to_string()),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            padding: Edges {
                top: Spacing::Px(0.0),
                right: Spacing::Px(0.0),
                bottom: Spacing::Px(keyboard_pad),
                left: Spacing::Px(0.0),
            },
            ..Style::default()
        },
        vec![Component::column(
            Style {
                width: Dimension::Px(card_w),
                padding: Edges::all(28.0),
                gap: 8.0,
                align_items: AlignItems::Center,
                background: sheet_fill(),
                border_width: 1.0,
                border_color: line_strong(),
                border_radius: 16.0,
                box_shadow: Some(shadow_lg()),
                box_shadows: vec![inset_highlight(15)],
                ..Style::default()
            },
            vec![
                Component::column(
                    Style {
                        width: Dimension::Px(64.0),
                        height: Dimension::Px(64.0),
                        border_radius: 20.0,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        background: Color::rgba(224, 122, 120, 31),
                        border_width: 1.0,
                        border_color: Color::rgba(224, 122, 120, 71),
                        ..Style::default()
                    },
                    vec![Component::svg_document(
                        glyph_svg("lock", "#ffd5d4"),
                        24,
                        24,
                        Style {
                            width: Dimension::Px(28.0),
                            height: Dimension::Px(28.0),
                            ..Style::default()
                        },
                    )],
                ),
                Component::text(
                    locale::t("locked"),
                    Style {
                        font_size: 32.0,
                        font_weight: 650,
                        color: ink_token(),
                        text_align: TextAlign::Center,
                        ..Style::default()
                    },
                ),
                Component::text(
                    subject,
                    Style {
                        font_size: 12.0,
                        color: muted_token(),
                        text_align: TextAlign::Center,
                        ..Style::default()
                    },
                ),
                Component::text(
                    locale::t("lock-body"),
                    Style {
                        font_size: 13.0,
                        color: muted_token(),
                        text_align: TextAlign::Center,
                        ..Style::default()
                    },
                ),
                passphrase,
                unlock,
                Component::text(
                    locale::t("lock-footnote"),
                    Style {
                        font_size: 12.0,
                        color: faint_token(),
                        text_align: TextAlign::Center,
                        ..Style::default()
                    },
                ),
            ],
        )],
    )
}

fn notice_row(notice: &ShellNotice) -> Component {
    let min_h = 48.0;
    let mut lines = vec![Component::text(
        notice.title.clone(),
        Style {
            font_size: 14.0,
            font_weight: 650,
            color: ink(),
            ..Style::default()
        },
    )];
    if !notice.detail.is_empty() {
        lines.push(Component::text(
            notice.detail.clone(),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    }
    let mut row = Component::column(
        Style {
            gap: 2.0,
            padding: Edges::xy(10.0, 8.0),
            width: Dimension::Percent(100.0),
            min_height: Dimension::Px(min_h),
            background: Color::rgba(244, 241, 234, 10),
            border_radius: 12.0,
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::Center,
            ..Style::default()
        },
        lines,
    );
    if !notice.draft.is_empty() {
        row.style.custom_properties = Some(HashMap::from([(
            "--w3cos-draft".to_string(),
            notice.draft.clone(),
        )]));
        row.style.color = Color::TRANSPARENT;
        row.style.overflow = Overflow::Hidden;
        row.kind = ComponentKind::Button {
            label: notice.title.clone(),
        };
        row.on_click = invoke(OP_NOTIFY_OPEN);
    }
    row
}

fn build_notify_panel() -> Component {
    let (width, height) = state::shell_viewport();
    let phone = posture() == Posture::Phone;
    let notices = NOTICES.with(|slot| slot.borrow().clone());
    let mut body = vec![Component::row(
        Style {
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            gap: 8.0,
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t("notify-title"),
                Style {
                    font_size: 16.0,
                    font_weight: 700,
                    color: ink(),
                    flex_grow: 1.0,
                    ..Style::default()
                },
            ),
            chrome_button(locale::t("close"), OP_NOTIFY_TOGGLE, "#3d5570"),
        ],
    )];
    if notices.is_empty() {
        body.push(Component::text(
            locale::t("notify-empty"),
            Style {
                font_size: 13.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for notice in notices.iter().rev().take(8) {
            body.push(notice_row(notice));
        }
    }
    if phone {
        let panel_h = (height * 0.46).clamp(160.0, 420.0);
        return Component::column(
            Style {
                position: Position::Absolute,
                left: Dimension::Px(0.0),
                top: Dimension::Px(0.0),
                width: Dimension::Px(width),
                height: Dimension::Px(panel_h),
                overflow: Overflow::Auto,
                padding: Edges::all(16.0),
                gap: 8.0,
                background: Color::from_hex("#1b2c3e"),
                border_bottom_left_radius: Some(22.0),
                border_bottom_right_radius: Some(22.0),
                z_index: CHROME_Z + 1,
                ..Style::default()
            },
            body,
        );
    }
    let panel_w = 320.0_f32.min((width - 32.0).max(220.0));
    let floor = intent_block_top(height) - 12.0;
    let rows = if notices.is_empty() {
        1
    } else {
        notices.len().min(6)
    };
    let desired = 56.0
        + rows as f32
            * (if state::pointer_is_coarse() {
                56.0
            } else {
                52.0
            });
    let panel_h = desired.min((floor - 16.0).max(96.0));
    let top = (floor - panel_h).max(8.0);
    Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px((width - panel_w - 16.0).max(8.0)),
            top: Dimension::Px(top),
            width: Dimension::Px(panel_w),
            height: Dimension::Px(panel_h),
            max_height: Dimension::Px(panel_h),
            overflow: Overflow::Auto,
            padding: Edges::all(14.0),
            gap: 8.0,
            background: Color::from_hex("#1b2c3e"),
            border_radius: 14.0,
            z_index: CHROME_Z + 1,
            ..Style::default()
        },
        body,
    )
}

fn build_stage() -> Component {
    let current = state::get_signal(APP_SIGNAL);
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    let mut ids: Vec<i64> = (1..=6)
        .filter(|id| resident(*id) && open & app_bit(*id) != 0 && minimized & app_bit(*id) == 0)
        .collect();
    ids.sort_by_key(|id| (windows::frame(*id).z, *id));
    let mut children = vec![build_desktop()];
    for id in ids {
        children.push(build_window_frame(id, current == id));
    }
    Component::column(
        Style {
            position: Position::Relative,
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_width: Dimension::Px(0.0),
            min_height: Dimension::Px(0.0),
            overflow: Overflow::Hidden,
            ..Style::default()
        },
        children,
    )
}

fn app_body(id: i64) -> Component {
    match id {
        1 => build_files_app(),
        2 => build_terminal_app(),
        3 => build_settings_app(),
        5 => build_browser_app(),
        6 => build_editor_app(),
        _ => Component::column(Style::default(), vec![]),
    }
}

fn build_window_frame(id: i64, focused: bool) -> Component {
    let frame = windows::frame(id);
    let mut window = Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(frame.x),
            top: Dimension::Px(frame.y),
            width: Dimension::Px(frame.w),
            height: Dimension::Px(frame.h),
            z_index: frame.z,
            overflow: Overflow::Hidden,
            box_sizing: BoxSizing::BorderBox,
            border_width: 1.0,
            border_color: if focused {
                Color::rgba(20, 150, 106, 210)
            } else {
                line_strong()
            },
            border_radius: 14.0,
            box_shadow: Some(shadow_lg()),
            box_shadows: vec![inset_highlight(15)],
            background: Color::from_hex("#1a242f"),
            flex_shrink: 0.0,
            ..Style::default()
        },
        vec![
            resize_handle(id, "n", Dimension::Percent(100.0), Dimension::Px(6.0)),
            Component::row(
                Style {
                    flex_grow: 1.0,
                    min_height: Dimension::Px(0.0),
                    overflow: Overflow::Hidden,
                    gap: 0.0,
                    ..Style::default()
                },
                vec![
                    resize_handle(id, "w", Dimension::Px(6.0), Dimension::Percent(100.0)),
                    Component::column(
                        Style {
                            flex_grow: 1.0,
                            min_width: Dimension::Px(0.0),
                            min_height: Dimension::Px(0.0),
                            overflow: Overflow::Auto,
                            ..Style::default()
                        },
                        vec![app_body(id)],
                    ),
                    Component::column(
                        Style {
                            width: Dimension::Px(16.0),
                            flex_shrink: 0.0,
                            gap: 0.0,
                            ..Style::default()
                        },
                        vec![
                            {
                                let mut east =
                                    resize_handle(id, "e", Dimension::Px(16.0), Dimension::Auto);
                                east.style.flex_grow = 1.0;
                                east
                            },
                            resize_corner(id),
                        ],
                    ),
                ],
            ),
            resize_handle(id, "s", Dimension::Percent(100.0), Dimension::Px(6.0)),
        ],
    );
    window.kind = ComponentKind::Button {
        label: String::new(),
    };
    window.on_click = invoke(focus_code(id));
    window.style.custom_properties = Some(win_props(id, None));
    window
}

fn win_props(id: i64, gesture: Option<&str>) -> HashMap<String, String> {
    let mut props = HashMap::from([("--w3cos-win".to_string(), id.to_string())]);
    if let Some(gesture) = gesture {
        props.insert("--w3cos-gesture".to_string(), gesture.to_string());
    }
    props
}

fn resize_handle(id: i64, gesture: &str, width: Dimension, height: Dimension) -> Component {
    let mut handle = Component::button_with_click(
        " ",
        Style {
            width,
            height,
            flex_shrink: 0.0,
            padding: Edges::ZERO,
            font_size: 1.0,
            color: Color::TRANSPARENT,
            background: Color::TRANSPARENT,
            ..Style::default()
        },
        invoke(focus_code(id)),
    );
    handle.style.custom_properties = Some(win_props(id, Some(gesture)));
    handle
}

fn resize_corner(id: i64) -> Component {
    let mut corner = Component::button_with_click(
        " ",
        Style {
            width: Dimension::Px(16.0),
            height: Dimension::Px(16.0),
            flex_shrink: 0.0,
            padding: Edges::ZERO,
            font_size: 1.0,
            color: Color::TRANSPARENT,
            background: Color::TRANSPARENT,
            ..Style::default()
        },
        invoke(focus_code(id)),
    );
    corner.style.custom_properties = Some(win_props(id, Some("se")));
    corner
}

// ==========================================================================
// Desktop (home screen)
// ==========================================================================

fn build_desktop() -> Component {
    let night = state::get_signal(NIGHT_SIGNAL) != 0;
    Component::column(
        Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_height: Dimension::Px(0.0),
            min_width: Dimension::Px(0.0),
            overflow: Overflow::Hidden,
            background: Color::from_hex(if night { "#101820" } else { "#13202c" }),
            background_image: Some(WALLPAPER.to_string()),
            padding: Edges::all(28.0),
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::FlexStart,
            ..Style::default()
        },
        vec![build_desktop_icons()],
    )
}

fn desktop_tile(glyph: &str, label: String, code: u8) -> Component {
    let (fill, stroke) = squircle_tone(glyph);
    let mut face = glyph_face(glyph, label.clone(), code, fill, stroke, 64.0, 18.0, false);
    face.style.border_width = 1.0;
    face.style.border_color = Color::rgba(244, 241, 234, 36);
    face.style.box_shadow = Some(BoxShadow::new(
        0.0,
        10.0,
        20.0,
        0.0,
        Color::rgba(0, 0, 0, 46),
    ));
    face.style.box_shadows = vec![inset_highlight(41)];
    Component::column(
        Style {
            width: Dimension::Px(if posture() == Posture::Phone {
                96.0
            } else {
                104.0
            }),
            align_items: AlignItems::Center,
            gap: 8.0,
            ..Style::default()
        },
        vec![
            face,
            Component::text(
                label,
                Style {
                    font_size: 13.0,
                    color: ink_token(),
                    text_align: TextAlign::Center,
                    text_shadows: label_shadow(),
                    ..Style::default()
                },
            ),
        ],
    )
}

fn desktop_icon(app: ResidentApp) -> Component {
    desktop_tile(app.glyph, app_label(app.id), focus_code(app.id))
}

fn build_desktop_icons() -> Component {
    let phone = posture() == Posture::Phone;
    let mut tiles: Vec<Component> = RESIDENT_APPS.iter().copied().map(desktop_icon).collect();
    if phone {
        tiles.push(desktop_tile(
            "devices",
            locale::t("settings-devices"),
            OP_SETTINGS_DEVICES,
        ));
    }
    let columns = if phone { 3 } else { 2 };
    let mut rows = Vec::new();
    for pair in tiles.chunks(columns) {
        rows.push(Component::row(
            Style {
                gap: 8.0,
                align_items: AlignItems::FlexStart,
                ..Style::default()
            },
            pair.to_vec(),
        ));
    }
    Component::column(
        Style {
            gap: 22.0,
            align_items: AlignItems::FlexStart,
            ..Style::default()
        },
        rows,
    )
}

fn build_shared_launcher() -> Component {
    Component::row(
        Style {
            gap: 16.0,
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![
            Component::button_with_click(
                locale::t("shared-hello"),
                Style {
                    background: Color::from_hex("#2bb0ed"),
                    color: Color::from_hex("#041422"),
                    border_radius: 8.0,
                    font_size: 14.0,
                    padding: Edges::xy(16.0, 10.0),
                    ..Style::default()
                },
                invoke(OP_SHARED_APP),
            ),
            Component::button_with_click(
                locale::t("shared-web"),
                Style {
                    background: Color::from_hex("#1b4332"),
                    color: Color::from_hex("#d8f3dc"),
                    border_radius: 8.0,
                    font_size: 14.0,
                    padding: Edges::xy(16.0, 10.0),
                    ..Style::default()
                },
                invoke(OP_SHARED_PAGE),
            ),
            Component::button_with_click(
                locale::t("shared-counter"),
                Style {
                    background: Color::from_hex("#9b2226"),
                    color: Color::from_hex("#fff1f0"),
                    border_radius: 8.0,
                    font_size: 14.0,
                    padding: Edges::xy(16.0, 10.0),
                    ..Style::default()
                },
                invoke(OP_SHARED_PLUGIN),
            ),
        ],
    )
}

// ==========================================================================
// Files App
// ==========================================================================

fn build_files_app() -> Component {
    let folder = state::get_signal(FILES_FOLDER_SIGNAL);
    let (title, crumbs) = folder_title(folder);
    let mut listing = folder_rows(folder);
    listing.push(Component::text(
        folder_summary(folder),
        Style {
            font_size: 11.0,
            color: Color::from_hex("#606080"),
            padding: Edges::all(8.0),
            ..Style::default()
        },
    ));
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#0f0f1a"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(title, "#3498db", 1),
            Component::row(
                Style {
                    flex_grow: 1.0,
                    gap: 0.0,
                    ..Style::default()
                },
                vec![
                    Component::column(
                        Style {
                            width: Dimension::Px(200.0),
                            background: Color::from_hex("#10101c"),
                            padding: Edges::all(12.0),
                            gap: 4.0,
                            ..Style::default()
                        },
                        vec![
                            nav_button(&locale::t("files-home"), folder == 0, OP_FILES_HOME),
                            nav_button(&locale::t("files-docs"), folder == 1, OP_FILES_DOCS),
                            nav_button(&locale::t("files-pics"), folder == 2, OP_FILES_PICS),
                            nav_button(&locale::t("files-down"), folder == 3, OP_FILES_DOWN),
                            nav_button(&locale::t("files-music"), folder == 4, OP_FILES_MUSIC),
                        ],
                    ),
                    Component::column(
                        Style {
                            flex_grow: 1.0,
                            padding: Edges::all(16.0),
                            gap: 4.0,
                            ..Style::default()
                        },
                        {
                            let mut rows = vec![Component::text(
                                crumbs,
                                Style {
                                    font_size: 13.0,
                                    color: Color::from_hex("#d5dee8"),
                                    padding: Edges::xy(8.0, 4.0),
                                    ..Style::default()
                                },
                            )];
                            rows.append(&mut listing);
                            rows
                        },
                    ),
                    file_preview_pane(),
                ],
            ),
        ],
    )
}

fn folder_title(folder: i64) -> (String, String) {
    match folder {
        1 => (locale::t("files-docs"), locale::t("files-crumb-docs")),
        2 => (locale::t("files-pics"), locale::t("files-crumb-pics")),
        3 => (locale::t("files-down"), locale::t("files-crumb-down")),
        4 => (locale::t("files-music"), locale::t("files-crumb-music")),
        5 => ("projects".to_string(), locale::t("files-crumb-projects")),
        6 => ("w3cos".to_string(), locale::t("files-crumb-w3cos")),
        _ => (locale::t("app-files"), locale::t("files-crumb-home")),
    }
}

fn folder_summary(folder: i64) -> String {
    let count = match folder {
        1 | 2 | 5 | 6 => 2,
        3 => 1,
        4 => 0,
        _ => 6,
    };
    let count = count.to_string();
    locale::t_args("files-count", &[("count", &count)])
}

fn folder_rows(folder: i64) -> Vec<Component> {
    match folder {
        1 => vec![
            file_row("notes.txt", &locale::t("file-text"), "1.2 KB", None),
            file_row("design.md", &locale::t("file-markdown"), "8.4 KB", None),
        ],
        2 => vec![
            file_row("screenshot.png", &locale::t("file-png"), "245 KB", None),
            file_row("wallpaper.png", &locale::t("file-png"), "1.1 MB", None),
        ],
        3 => vec![file_row(
            "fetch-l2-models.sh",
            &locale::t("file-script"),
            "2.0 KB",
            None,
        )],
        4 => vec![Component::text(
            locale::t("files-empty"),
            Style {
                font_size: 14.0,
                color: Color::from_hex("#d5dee8"),
                padding: Edges::all(12.0),
                ..Style::default()
            },
        )],
        5 => vec![
            file_row("README.md", &locale::t("file-markdown"), "4.2 KB", None),
            file_row("Cargo.toml", &locale::t("file-toml"), "1.1 KB", None),
        ],
        6 => vec![
            file_row("app.tsx", &locale::t("file-typescript"), "2.8 KB", None),
            file_row("shell.rs", &locale::t("file-rust"), "18 KB", None),
        ],
        _ => vec![
            file_row(
                "projects",
                &locale::t("file-folder"),
                "--",
                Some(OP_FILES_PROJECTS),
            ),
            file_row(
                "w3cos",
                &locale::t("file-folder"),
                "--",
                Some(OP_FILES_W3COS),
            ),
            file_row("README.md", &locale::t("file-markdown"), "4.2 KB", None),
            file_row("Cargo.toml", &locale::t("file-toml"), "1.1 KB", None),
            file_row("app.tsx", &locale::t("file-typescript"), "2.8 KB", None),
            file_row("screenshot.png", &locale::t("file-png"), "245 KB", None),
        ],
    }
}

fn file_preview_pane() -> Component {
    if !matches!(posture(), Posture::WideTablet) {
        return Component::column(Style::default(), vec![]);
    }
    let selected = state::get_signal_text(FILE_PREVIEW_SIGNAL);
    let body = if selected.is_empty() {
        locale::t("files-preview-empty")
    } else {
        selected
    };
    Component::column(
        Style {
            width: Dimension::Px(280.0),
            flex_shrink: 0.0,
            background: Color::from_hex("#1c2631"),
            padding: Edges::all(16.0),
            gap: 12.0,
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t("files-preview"),
                Style {
                    font_size: 16.0,
                    font_weight: 700,
                    color: ink(),
                    ..Style::default()
                },
            ),
            Component::text(
                body,
                Style {
                    font_size: 14.0,
                    color: muted_ink(),
                    ..Style::default()
                },
            ),
        ],
    )
}

fn nav_button(name: &str, active: bool, code: u8) -> Component {
    let mut button = Component::button_with_click(
        name,
        Style {
            font_size: 14.0,
            font_weight: if active { 700 } else { 500 },
            color: Color::from_hex(if active { "#d7f6e8" } else { "#d5dee8" }),
            background: Color::from_hex(if active { "#14362c" } else { "#243447" }),
            border_width: if active { 1.0 } else { 0.0 },
            border_color: Color::from_hex("#1c8f62"),
            border_radius: 10.0,
            padding: Edges::xy(12.0, 0.0),
            min_height: Dimension::Px(48.0),
            width: Dimension::Percent(100.0),
            ..Style::default()
        },
        invoke(code),
    );
    if active {
        let props = button
            .style
            .custom_properties
            .get_or_insert_with(HashMap::new);
        props.insert("--w3cos-selected".to_string(), "1".to_string());
    }
    button
}

fn settings_switch(label: &str, on: bool, code: u8) -> Component {
    let mut button = Component::button_with_click(
        label,
        Style {
            font_size: 15.0,
            font_weight: 650,
            color: Color::from_hex(if on { "#e9fff5" } else { "#f4f7fb" }),
            background: Color::from_hex(if on { "#0c6d4c" } else { "#31475e" }),
            border_radius: 12.0,
            padding: Edges::xy(14.0, 0.0),
            min_height: Dimension::Px(48.0),
            width: Dimension::Percent(100.0),
            ..Style::default()
        },
        invoke(code),
    );
    let props = button
        .style
        .custom_properties
        .get_or_insert_with(HashMap::new);
    props.insert(
        "--w3cos-switch".to_string(),
        if on { "1" } else { "0" }.to_string(),
    );
    button
}

fn settings_link(label: String, code: u8) -> Component {
    let mut button = chrome_button(label, code, "#31475e");
    button.style.min_height = Dimension::Px(48.0);
    button.style.height = Dimension::Px(48.0);
    button.style.width = Dimension::Percent(100.0);
    button
}

fn file_row(name: &str, kind: &str, size: &str, open: Option<u8>) -> Component {
    let icon = if open.is_some() { "[DIR]" } else { "    " };
    let name_style = Style {
        font_size: 13.0,
        color: Color::from_hex(if open.is_some() { "#9eb6ff" } else { "#d0d0e0" }),
        flex_grow: 1.0,
        ..Style::default()
    };
    let mut name_node =
        Component::button_with_click(name, name_style, invoke(open.unwrap_or(OP_PREVIEW)));
    name_node.style.min_height = Dimension::Px(48.0);
    if open.is_none() {
        name_node.style.custom_properties = Some(HashMap::from([(
            "--w3cos-draft".to_string(),
            name.to_string(),
        )]));
    }
    Component::row(
        Style {
            padding: Edges::xy(8.0, 8.0),
            gap: 16.0,
            border_radius: 10.0,
            align_items: AlignItems::Center,
            min_height: Dimension::Px(48.0),
            ..Style::default()
        },
        vec![
            Component::text(
                icon,
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#1c8f62"),
                    ..Style::default()
                },
            ),
            name_node,
            Component::text(
                size,
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#808090"),
                    ..Style::default()
                },
            ),
            Component::text(
                kind,
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#808090"),
                    ..Style::default()
                },
            ),
        ],
    )
}

// ==========================================================================
// Terminal App
// ==========================================================================

fn build_terminal_app() -> Component {
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#24384a"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(locale::t("app-terminal"), "#1c8f62", 2),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    padding: Edges::all(16.0),
                    gap: 10.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        locale::t("terminal-ready"),
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#b7f0d8"),
                            ..Style::default()
                        },
                    ),
                    Component::row(
                        Style {
                            gap: 8.0,
                            padding: Edges::xy(12.0, 10.0),
                            background: Color::from_hex("#1b2c3e"),
                            border_radius: 8.0,
                            align_items: AlignItems::Center,
                            ..Style::default()
                        },
                        vec![
                            Component::text(
                                locale::t("terminal-prompt"),
                                Style {
                                    font_size: 14.0,
                                    color: Color::from_hex("#9eb6ff"),
                                    ..Style::default()
                                },
                            ),
                            {
                                let mut term_style = Style {
                                    font_size: 14.0,
                                    color: Color::from_hex("#f4f7fb"),
                                    background: Color::from_hex("#1b2c3e"),
                                    flex_grow: 1.0,
                                    ..Style::default()
                                };
                                term_style.custom_properties = Some(HashMap::from([(
                                    "--w3cos-ime".to_string(),
                                    "terminal".to_string(),
                                )]));
                                note_preedit(&mut term_style, "terminal");
                                Component::text_input(
                                    state::get_signal_text(TERMINAL_SIGNAL),
                                    "help",
                                    term_style,
                                )
                            },
                            chrome_button(locale::t("terminal-run"), OP_RUN_TERMINAL, "#1c8f62"),
                        ],
                    ),
                    Component::text(
                        {
                            let output = state::get_signal_text(TERM_OUT_SIGNAL);
                            if output.is_empty() {
                                locale::t("terminal-hint")
                            } else {
                                output
                            }
                        },
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#b7f0d8"),
                            ..Style::default()
                        },
                    ),
                ],
            ),
        ],
    )
}

// ==========================================================================
// Settings App
// ==========================================================================

fn settings_nav(tab: i64, mark_current: bool) -> Vec<Component> {
    let on = |value: i64| mark_current && tab == value;
    vec![
        nav_button(&locale::t("settings-local"), on(0), OP_SETTINGS_LOCAL),
        nav_button(&locale::t("settings-identity"), on(1), OP_SETTINGS_IDENTITY),
        nav_button(
            &locale::t("settings-language-nav"),
            on(6),
            OP_SETTINGS_LANGUAGE,
        ),
        nav_button(&locale::t("settings-agent"), on(2), OP_SETTINGS_BRIDGE),
        nav_button(
            &locale::t("settings-devices-nav"),
            on(4),
            OP_SETTINGS_DEVICES,
        ),
    ]
}

fn build_settings_app() -> Component {
    let tab = state::get_signal(SETTINGS_TAB_SIGNAL);
    let phone = posture() == Posture::Phone;
    let menu = state::get_signal(SETTINGS_MENU_SIGNAL) != 0;
    if phone && menu {
        return Component::column(
            Style {
                flex_grow: 1.0,
                background: Color::from_hex("#24384a"),
                padding: Edges::all(12.0),
                gap: 8.0,
                ..Style::default()
            },
            settings_nav(tab, false),
        );
    }
    let pane = Component::column(
        Style {
            flex_grow: 1.0,
            min_width: Dimension::Px(0.0),
            padding: Edges::all(if phone { 16.0 } else { 20.0 }),
            gap: 12.0,
            overflow: Overflow::Auto,
            ..Style::default()
        },
        settings_page(tab),
    );
    if phone {
        let mut back = chrome_button(locale::t("back"), OP_SETTINGS_BACK, "#31475e");
        back.style.min_height = Dimension::Px(48.0);
        back.style.width = Dimension::Percent(100.0);
        return Component::column(
            Style {
                flex_grow: 1.0,
                background: Color::from_hex("#24384a"),
                padding: Edges::all(12.0),
                gap: 12.0,
                ..Style::default()
            },
            vec![back, pane],
        );
    }
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#24384a"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(locale::t("app-settings"), "#d4a017", 3),
            Component::row(
                Style {
                    flex_grow: 1.0,
                    min_height: Dimension::Px(0.0),
                    gap: 0.0,
                    ..Style::default()
                },
                vec![
                    Component::column(
                        Style {
                            width: Dimension::Px(200.0),
                            flex_shrink: 0.0,
                            background: Color::from_hex("#1b2c3e"),
                            padding: Edges::all(12.0),
                            gap: 4.0,
                            ..Style::default()
                        },
                        settings_nav(tab, true),
                    ),
                    pane,
                ],
            ),
        ],
    )
}

fn settings_page(tab: i64) -> Vec<Component> {
    match tab {
        1 => settings_identity_page(),
        2 => settings_agent_page(),
        3 => settings_l2_page(),
        4 => settings_devices_page(),
        5 => settings_lexicon_page(),
        6 => settings_language_page(),
        _ => settings_local_page(),
    }
}

fn device_line(label: String, color: Color) -> Component {
    let text = Component::text(
        label,
        Style {
            font_size: 14.0,
            color,
            ..Style::default()
        },
    );
    if state::pointer_is_coarse() {
        Component::row(
            Style {
                min_height: Dimension::Px(48.0),
                align_items: AlignItems::Center,
                ..Style::default()
            },
            vec![text],
        )
    } else {
        text
    }
}

fn settings_devices_page() -> Vec<Component> {
    let mut page = settings_heading(
        &locale::t("settings-devices-nav"),
        &locale::t("settings-devices-detail"),
    );
    let fingerprint = truncate_middle(&handoff::local_fingerprint(), 28);
    page.push(device_line(
        format!("{} · {fingerprint}", handoff::local_name()),
        ink(),
    ));
    let qr = handoff::pairing_qr_svg();
    if qr.contains("<svg") {
        page.push(Component::svg_document(
            qr,
            132,
            132,
            Style {
                width: Dimension::Px(132.0),
                height: Dimension::Px(132.0),
                ..Style::default()
            },
        ));
    }
    if let Some(peer) = handoff::pending_peer() {
        let (grouped, spaced) = handoff::short_code(
            &handoff::local_fingerprint(),
            &peer.fingerprint,
            &peer.nonce,
        );
        page.push(device_line(
            format!("{} · {}", peer.name, locale::t("pair-pending")),
            Color::from_hex("#f0d7b4"),
        ));
        page.push(Component::text(
            grouped,
            Style {
                font_size: 28.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ));
        page.push(Component::text(
            spaced,
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
        page.push(chrome_button(
            locale::t("pair-mismatch-button"),
            OP_PAIR_NO,
            "#9a3412",
        ));
        page.push(chrome_button(
            locale::t("pair-trust-button"),
            OP_PAIR_YES,
            "#0c6d4c",
        ));
    } else if handoff::peers().is_empty() {
        page.push(Component::text(
            locale::t("devices-empty"),
            Style {
                font_size: 14.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for peer in handoff::peers() {
            let state = if peer.trusted {
                format!(
                    "{} · {}",
                    locale::t("pair-trusted-short"),
                    locale::t(peer.link.message_id())
                )
            } else {
                locale::t("pair-pending").to_string()
            };
            page.push(device_line(format!("{} · {state}", peer.name), ink()));
        }
    }
    let placeholder = locale::t("pair-paste-placeholder");
    page.push(Component::text_input(
        "",
        placeholder.clone(),
        Style {
            min_height: Dimension::Px(touch_min()),
            font_size: 14.0,
            color: ink(),
            background: Color::from_hex("#101820"),
            border_radius: 10.0,
            padding: Edges::xy(12.0, 8.0),
            ..Style::default()
        },
    ));
    let mut import = chrome_button(locale::t("pair-paste-button"), OP_PAIR_IMPORT, "#3d5570");
    import.on_click = EventAction::Invoke {
        code: OP_PAIR_IMPORT,
        input_placeholder: placeholder,
    };
    page.push(import);
    if let Some(kind) = handoff::banner_kind() {
        let copy = match kind {
            handoff::BannerKind::Sending => locale::t("handoff-sending"),
            handoff::BannerKind::NoAnswer => handoff_notice_text("handoff-no-answer"),
            handoff::BannerKind::Offer => String::new(),
        };
        if !copy.is_empty() {
            page.push(Component::text(
                copy,
                Style {
                    font_size: 14.0,
                    color: ink(),
                    ..Style::default()
                },
            ));
        }
    }
    let notice = handoff::notice();
    if !notice.is_empty() && notice != "handoff-sending" && notice != "handoff-no-answer" {
        page.push(Component::text(
            handoff_notice_text(&notice),
            Style {
                font_size: 13.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    }
    page
}

fn banner_anchor() -> (f32, f32, f32) {
    let (width, _) = state::shell_viewport();
    let title_h = if state::pointer_is_coarse() {
        56.0
    } else {
        40.0
    };
    if posture() == Posture::Desktop {
        let id = state::get_signal(APP_SIGNAL).clamp(1, 6);
        let frame = windows::frame(id);
        let top = frame.y + 6.0 + title_h;
        let banner_w = (frame.w - 12.0).max(280.0);
        (frame.x + 6.0, top, banner_w)
    } else {
        let left = 80.0;
        ((left), title_h, (width - left - 16.0).max(280.0))
    }
}

fn handoff_strip(children: Vec<Component>, width: f32) -> Component {
    Component::row(
        Style {
            width: Dimension::Px(width),
            padding: Edges::xy(14.0, 10.0),
            gap: 12.0,
            align_items: AlignItems::Center,
            background: Color::from_hex("#1c2631"),
            border_radius: 12.0,
            border_width: 1.0,
            border_color: Color::rgba(244, 241, 234, 31),
            ..Style::default()
        },
        children,
    )
}

fn build_handoff_banner() -> Component {
    let (left, top, width) = banner_anchor();
    let kind = handoff::banner_kind().unwrap_or(handoff::BannerKind::Offer);
    let (copy, buttons) = match kind {
        handoff::BannerKind::Offer => (
            vec![
                Component::text(
                    locale::t("handoff-title"),
                    Style {
                        font_size: 15.0,
                        font_weight: 700,
                        color: ink(),
                        ..Style::default()
                    },
                ),
                Component::text(
                    locale::t("handoff-detail"),
                    Style {
                        font_size: 12.0,
                        color: muted_ink(),
                        ..Style::default()
                    },
                ),
                Component::text(
                    locale::t(handoff::offer_link().message_id()),
                    Style {
                        font_size: 12.0,
                        font_weight: 650,
                        color: Color::from_hex("#f0d7b4"),
                        ..Style::default()
                    },
                ),
            ],
            vec![
                chrome_button(locale::t("handoff-later"), OP_HANDOFF_LATER, "#3d5570"),
                chrome_button(locale::t("handoff-go"), OP_HANDOFF_GO, "#0c6d4c"),
            ],
        ),
        handoff::BannerKind::Sending => (
            vec![Component::text(
                locale::t("handoff-sending"),
                Style {
                    font_size: 14.0,
                    color: ink(),
                    ..Style::default()
                },
            )],
            vec![chrome_button(
                locale::t("handoff-cancel"),
                OP_HANDOFF_CANCEL,
                "#9a3412",
            )],
        ),
        handoff::BannerKind::NoAnswer => (
            vec![Component::text(
                handoff_notice_text("handoff-no-answer"),
                Style {
                    font_size: 14.0,
                    color: ink(),
                    ..Style::default()
                },
            )],
            vec![chrome_button(
                locale::t("handoff-later"),
                OP_HANDOFF_LATER,
                "#3d5570",
            )],
        ),
    };
    let mut strip = handoff_strip(
        vec![
            Component::column(
                Style {
                    flex_grow: 1.0,
                    gap: 2.0,
                    min_width: Dimension::Px(0.0),
                    ..Style::default()
                },
                copy,
            ),
            Component::row(
                Style {
                    gap: 8.0,
                    flex_shrink: 0.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                buttons,
            ),
        ],
        width,
    );
    strip.style.position = Position::Absolute;
    strip.style.left = Dimension::Px(left);
    strip.style.top = Dimension::Px(top);
    strip.style.z_index = CHROME_Z + 1;
    strip
}

fn build_sync_sheet() -> Component {
    let surface = handoff::sync_prompt().unwrap_or(handoff::SyncSurface::Receipts);
    let (title, detail) = match surface {
        handoff::SyncSurface::Receipts => ("sync-receipts-title", "sync-receipts-detail"),
        handoff::SyncSurface::Drafts => ("sync-drafts-title", "sync-drafts-detail"),
        handoff::SyncSurface::Scene => ("sync-scene-title", "sync-scene-detail"),
        handoff::SyncSurface::Clipboard => ("sync-clipboard-title", "sync-clipboard-detail"),
    };
    dialog_shell(vec![
        Component::text(
            locale::t(title),
            Style {
                font_size: 18.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ),
        Component::text(
            locale::t(detail),
            Style {
                font_size: 13.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        Component::row(
            Style {
                gap: 8.0,
                justify_content: JustifyContent::FlexEnd,
                ..Style::default()
            },
            vec![
                sheet_button(locale::t("sync-ask-skip"), OP_SYNC_SKIP, SheetKind::Danger),
                sheet_button(
                    locale::t("sync-ask-allow"),
                    OP_SYNC_ALLOW,
                    SheetKind::Primary,
                ),
            ],
        ),
    ])
}

fn clip_sheet_rows() -> Vec<Component> {
    let Some((local, remote)) = handoff::clip_conflict() else {
        return Vec::new();
    };
    let row = |side: &handoff::ClipSide, code: u8| {
        Component::column(
            Style {
                gap: 4.0,
                padding: Edges::all(10.0),
                border_radius: 12.0,
                background: Color::rgba(244, 241, 234, 10),
                ..Style::default()
            },
            vec![
                Component::text(
                    locale::t_args(
                        "clip-conflict-row",
                        &[("device", &side.device), ("when", &clip_clock(side.unix))],
                    ),
                    Style {
                        font_size: 13.0,
                        font_weight: 650,
                        color: ink(),
                        ..Style::default()
                    },
                ),
                Component::text(
                    side.text.clone(),
                    Style {
                        font_size: 13.0,
                        color: muted_ink(),
                        ..Style::default()
                    },
                ),
                chrome_button(locale::t("clip-use"), code, "#0c6d4c"),
            ],
        )
    };
    vec![
        Component::text(
            locale::t("clip-conflict-title"),
            Style {
                font_size: 16.0,
                font_weight: 700,
                color: Color::from_hex("#f0d7b4"),
                ..Style::default()
            },
        ),
        Component::text(
            locale::t("clip-conflict-detail"),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        row(&local, OP_CLIP_MINE),
        row(&remote, OP_CLIP_THEIRS),
    ]
}

fn build_clip_dock() -> Component {
    let (width, height) = state::shell_viewport();
    let dock_w = 420.0_f32.min((width - 24.0).max(280.0));
    let mut card = Component::column(
        Style {
            width: Dimension::Px(dock_w),
            padding: Edges::all(14.0),
            gap: 8.0,
            background: Color::from_hex("#1c2631"),
            border_radius: 16.0,
            border_width: 1.0,
            border_color: Color::rgba(244, 241, 234, 31),
            ..Style::default()
        },
        clip_sheet_rows(),
    );
    card.style.position = Position::Absolute;
    card.style.left = Dimension::Px(((width - dock_w) / 2.0).max(12.0));
    card.style.bottom = Dimension::Px(24.0);
    card.style.top = Dimension::Auto;
    card.style.z_index = CHROME_Z + 1;
    card.style.max_height = Dimension::Px((height * 0.42).max(160.0));
    card
}

fn build_handoff_notice() -> Component {
    let (from, _, _) = handoff::incoming().unwrap_or_default();
    let (width, _) = state::shell_viewport();
    let card_w = 420.0_f32.min((width - 24.0).max(240.0));
    let mut card = Component::row(
        Style {
            width: Dimension::Px(card_w),
            padding: Edges::xy(14.0, 12.0),
            gap: 12.0,
            align_items: AlignItems::Center,
            background: Color::from_hex("#1c2631"),
            border_radius: 16.0,
            border_width: 1.0,
            border_color: Color::rgba(240, 215, 180, 90),
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t_args("handoff-incoming", &[("name", &from)]),
                Style {
                    flex_grow: 1.0,
                    font_size: 14.0,
                    font_weight: 650,
                    color: ink(),
                    ..Style::default()
                },
            ),
            chrome_button(locale::t("handoff-reveal"), OP_HANDOFF_REVEAL, "#0c6d4c"),
        ],
    );
    card.style.position = Position::Absolute;
    card.style.left = Dimension::Px(12.0);
    card.style.top = Dimension::Px(12.0);
    card.style.z_index = CHROME_Z + 1;
    card
}

fn handoff_page_body(from: &str, path: &str, draft: &str) -> Vec<Component> {
    let mut rows = vec![
        Component::text(
            locale::t_args("handoff-from", &[("name", from)]),
            Style {
                font_size: 22.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ),
        Component::text(
            path.to_string(),
            Style {
                font_size: 14.0,
                color: ink(),
                ..Style::default()
            },
        ),
        Component::text(
            draft.to_string(),
            Style {
                font_size: 14.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        Component::text(
            locale::t_args(
                "handoff-l2-still-off",
                &[("here", &handoff::local_name()), ("there", from)],
            ),
            Style {
                font_size: 13.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        Component::row(
            Style {
                gap: 8.0,
                ..Style::default()
            },
            vec![
                chrome_button(locale::t("handoff-open"), OP_HANDOFF_OPEN, "#0c6d4c"),
                chrome_button(locale::t("handoff-later"), OP_HANDOFF_HIDE, "#3d5570"),
            ],
        ),
    ];
    if handoff::clip_conflict().is_some() {
        rows.push(Component::column(
            Style {
                gap: 8.0,
                margin: Edges {
                    top: Spacing::Px(8.0),
                    ..Edges::ZERO
                },
                ..Style::default()
            },
            clip_sheet_rows(),
        ));
    }
    rows
}

fn build_handoff_incoming() -> Component {
    if posture() == Posture::Phone && !handoff::incoming_revealed() {
        return build_handoff_notice();
    }
    let (from, path, draft) = handoff::incoming().unwrap_or_default();
    let (width, height) = state::shell_viewport();
    let phone = posture() == Posture::Phone;
    let mut card = Component::column(
        Style {
            padding: Edges::all(if phone { 20.0 } else { 16.0 }),
            gap: 12.0,
            background: Color::from_hex("#1c2631"),
            border_radius: if phone { 0.0 } else { 16.0 },
            overflow: Overflow::Auto,
            ..Style::default()
        },
        handoff_page_body(&from, &path, &draft),
    );
    card.style.position = Position::Absolute;
    card.style.z_index = CHROME_Z + 1;
    if phone {
        card.style.left = Dimension::Px(0.0);
        card.style.top = Dimension::Px(0.0);
        card.style.width = Dimension::Px(width);
        card.style.height = Dimension::Px(height);
    } else {
        let card_w = 440.0_f32.min((width - 32.0).max(280.0));
        card.style.left = Dimension::Px(((width - card_w) / 2.0).max(16.0));
        card.style.top = Dimension::Px(72.0);
        card.style.width = Dimension::Px(card_w);
        card.style.max_height = Dimension::Px((height - 120.0).max(280.0));
    }
    card
}

fn settings_heading(title: &str, detail: &str) -> Vec<Component> {
    vec![
        Component::text(
            title,
            Style {
                font_size: 18.0,
                color: Color::from_hex("#f4f7fb"),
                font_weight: 700,
                ..Style::default()
            },
        ),
        Component::text(
            detail,
            Style {
                font_size: 13.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        ),
    ]
}

fn settings_lexicon_page() -> Vec<Component> {
    let mut rows = settings_heading(
        &locale::t("settings-lexicon-title"),
        &locale::t("settings-lexicon-detail"),
    );
    if !lexicon::unlocked() {
        rows.push(Component::text(
            locale::t("lexicon-locked"),
            Style {
                font_size: 14.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        ));
        return rows;
    }
    rows.extend(lexicon_entry_rows());
    let stealth_label = if lexicon::stealth_on() {
        locale::t("lexicon-stealth-on")
    } else {
        locale::t("lexicon-stealth-off")
    };
    let app = state::get_signal(APP_SIGNAL);
    let hidden = if lexicon::app_hidden(app_key(app)) {
        locale::t("on")
    } else {
        locale::t("off")
    };
    rows.push(lexicon_pair(
        chrome_button(stealth_label, OP_LEXICON_STEALTH, "#3d5570"),
        chrome_button(
            format!("{} · {hidden}", locale::t("lexicon-stealth-app")),
            OP_LEXICON_APP,
            "#3d5570",
        ),
    ));
    rows.push(lexicon_draft_field());
    let placeholder = locale::t("lexicon-entry-placeholder");
    let mut save = chrome_button(locale::t("lexicon-save"), OP_LEXICON_SAVE, "#0c6d4c");
    save.on_click = EventAction::Invoke {
        code: OP_LEXICON_SAVE,
        input_placeholder: placeholder.clone(),
    };
    let mut drop = chrome_button(locale::t("lexicon-drop"), OP_LEXICON_DROP, "#3d5570");
    drop.on_click = EventAction::Invoke {
        code: OP_LEXICON_DROP,
        input_placeholder: placeholder,
    };
    rows.push(lexicon_pair(save, drop));
    rows.push(lexicon_pair(
        chrome_button(locale::t("lexicon-export"), OP_LEXICON_EXPORT, "#3d5570"),
        chrome_button(locale::t("lexicon-clear"), OP_LEXICON_CLEAR, "#9a3412"),
    ));
    rows
}

fn lexicon_pair(mut left: Component, mut right: Component) -> Component {
    left.style.flex_grow = 1.0;
    right.style.flex_grow = 1.0;
    Component::row(
        Style {
            gap: 8.0,
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![left, right],
    )
}

fn lexicon_entry_rows() -> Vec<Component> {
    let entries = lexicon::entries();
    if entries.is_empty() {
        return vec![Component::text(
            locale::t("lexicon-empty"),
            Style {
                font_size: 14.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        )];
    }
    entries
        .iter()
        .take(8)
        .map(|entry| {
            let line = if entry.kind == w3cos_ime::EntryKind::Alias {
                locale::t_args(
                    "lexicon-alias",
                    &[("alias", &entry.text), ("target", &entry.extra)],
                )
            } else {
                let freq = entry.freq.to_string();
                locale::t_args(
                    "lexicon-row",
                    &[
                        ("text", &entry.text),
                        ("extra", &entry.extra),
                        ("freq", &freq),
                    ],
                )
            };
            Component::text(
                line,
                Style {
                    font_size: 14.0,
                    color: Color::from_hex("#f4f7fb"),
                    ..Style::default()
                },
            )
        })
        .collect()
}

fn lexicon_draft_field() -> Component {
    let mut style = Style {
        font_size: 13.0,
        color: ink_token(),
        background: Color::rgba(244, 241, 234, 18),
        border_radius: 10.0,
        border_width: 1.0,
        border_color: Color::rgba(244, 241, 234, 31),
        padding: Edges::xy(10.0, 6.0),
        min_height: Dimension::Px(if state::pointer_is_coarse() {
            48.0
        } else {
            32.0
        }),
        ..Style::default()
    };
    style.custom_properties = Some(HashMap::from([(
        "--w3cos-ime".to_string(),
        "text".to_string(),
    )]));
    note_preedit(&mut style, "text");
    Component::text_input(
        state::get_signal_text(LEXICON_DRAFT_SIGNAL),
        locale::t("lexicon-entry-placeholder"),
        style,
    )
}

fn settings_local_page() -> Vec<Component> {
    let mut rows = settings_heading(
        &locale::t("settings-local-title"),
        &locale::t("settings-local-detail"),
    );
    rows.push(settings_switch(
        &locale::t("settings-night"),
        state::get_signal(NIGHT_SIGNAL) != 0,
        OP_CHIP_NIGHT,
    ));
    rows.push(settings_link(
        locale::t("settings-lexicon-open"),
        OP_SETTINGS_LEXICON,
    ));
    rows
}

fn settings_language_page() -> Vec<Component> {
    let mut rows = settings_heading(
        &locale::t("settings-language-title"),
        &locale::t("settings-language-detail"),
    );
    let pending = state::get_signal_text(PENDING_LOCALE_SIGNAL);
    rows.push(locale_choice("zh-CN", &pending));
    rows.push(locale_choice("en", &pending));
    rows.push(settings_row(
        &locale::t("ime-voice-model"),
        &locale::t(ime_bar::speech_model_status_key()),
    ));
    if let Some(key) = ime_bar::speech_model_button_key() {
        rows.push(settings_link(locale::t(key), OP_IME_MODEL));
    }
    rows.push(settings_row(
        &locale::t("ime-ink-model"),
        &locale::t(ime_bar::ink_model_status_key()),
    ));
    if let Some(key) = ime_bar::ink_model_button_key() {
        rows.push(settings_link(locale::t(key), OP_IME_INK_MODEL));
    }
    rows.push(settings_row(
        &locale::t("ime-screen-model"),
        &locale::t(ime_bar::screen_model_status_key()),
    ));
    if let Some(key) = ime_bar::screen_model_button_key() {
        rows.push(settings_link(locale::t(key), OP_IME_SCREEN_MODEL));
    }
    rows
}

fn locale_choice(tag: &str, pending: &str) -> Component {
    let current = locale::current() == tag;
    let name = locale_self_name(tag);
    let code = if tag == "en" {
        OP_LOCALE_EN
    } else {
        OP_LOCALE_ZH
    };
    let mut name_button = nav_button(&name, current, code);
    name_button.style.flex_grow = 1.0;
    name_button.style.width = Dimension::Auto;
    let tag_label = if tag == "en" {
        locale::t("locale-tag-en")
    } else {
        locale::t("locale-tag-zh")
    };
    let mut side = vec![Component::text(
        tag_label,
        Style {
            font_size: 12.0,
            color: muted_ink(),
            flex_shrink: 0.0,
            ..Style::default()
        },
    )];
    if pending == tag {
        side.push(Component::text(
            locale::t("locale-pending"),
            Style {
                font_size: 12.0,
                font_weight: 700,
                color: Color::from_hex("#f0d7b4"),
                flex_shrink: 0.0,
                ..Style::default()
            },
        ));
    }
    Component::row(
        Style {
            min_height: Dimension::Px(48.0),
            gap: 8.0,
            align_items: AlignItems::Center,
            width: Dimension::Percent(100.0),
            ..Style::default()
        },
        vec![
            name_button,
            Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    flex_shrink: 0.0,
                    ..Style::default()
                },
                side,
            ),
        ],
    )
}

fn settings_identity_page() -> Vec<Component> {
    let storage_owned = state::get_signal_text(STORAGE_SIGNAL);
    let storage_label = if storage_owned.is_empty() {
        "memory".to_string()
    } else {
        storage_owned
    };
    let subject_owned = state::get_signal_text(SUBJECT_SIGNAL);
    let subject_label = if subject_owned.is_empty() {
        LocalIdentityStub::SUBJECT.to_string()
    } else {
        subject_owned
    };
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut rows = settings_heading(
        &locale::t("settings-identity-title"),
        &locale::t("settings-identity-detail"),
    );
    let keep = if state::shell_viewport().0 < 700.0 {
        16
    } else {
        28
    };
    let verified = state::get_signal_text(ENROLL_SIGNAL) == "verified";
    rows.push(settings_row(
        &locale::t("passkey-did"),
        &truncate_middle(&subject_label, keep),
    ));
    rows.push(settings_row(
        &locale::t("passkey-fingerprint"),
        &truncate_middle(&handoff::local_fingerprint(), keep),
    ));
    rows.push(chrome_button(
        locale::t("settings-copy"),
        OP_COPY_SUBJECT,
        "#31475e",
    ));
    rows.push(Component::text(
        if verified {
            locale::t("passkey-verified")
        } else {
            locale::t("passkey-unregistered")
        },
        Style {
            font_size: 14.0,
            font_weight: 650,
            color: Color::from_hex("#ffe08a"),
            ..Style::default()
        },
    ));
    rows.push(chrome_button(
        locale::t("passkey-enroll"),
        OP_ENROLL,
        "#3d5570",
    ));
    if !verified {
        rows.push(settings_row(
            &locale::t("settings-identity-title"),
            &locale::t("settings-not-passkey"),
        ));
    }
    let status = if unlocked {
        locale::t("unlocked")
    } else {
        locale::t("locked")
    };
    rows.push(settings_row(&locale::t("settings-status"), &status));
    if unlocked {
        rows.push(chrome_button(locale::t("lock"), OP_LOCK, "#3d5570"));
    }
    rows.push(settings_row(&locale::t("settings-storage"), &storage_label));
    rows.push(Component::text(
        locale::t("chain-rpc-label"),
        Style {
            font_size: 14.0,
            font_weight: 650,
            color: Color::from_hex("#f4f7fb"),
            ..Style::default()
        },
    ));
    rows.push(chain_rpc_field());
    let mut save_rpc = chrome_button(locale::t("chain-rpc-save"), OP_CHAIN_SAVE, "#3d5570");
    save_rpc.on_click = EventAction::Invoke {
        code: OP_CHAIN_SAVE,
        input_placeholder: locale::t("chain-rpc-placeholder"),
    };
    rows.push(save_rpc);
    rows.push(sign_message_field());
    let mut sign = chrome_button(locale::t("sign-open"), OP_SIGN_PROPOSE, "#9a5410");
    sign.on_click = EventAction::Invoke {
        code: OP_SIGN_PROPOSE,
        input_placeholder: locale::t("sign-message-placeholder"),
    };
    rows.push(sign);
    rows.push(chrome_button(
        locale::t("open-ledger"),
        OP_TOGGLE_RECEIPTS,
        "#3d5570",
    ));
    rows
}

fn chain_rpc_field() -> Component {
    let coarse = state::pointer_is_coarse();
    Component::text_input(
        state::get_signal_text(CHAIN_RPC_SIGNAL),
        locale::t("chain-rpc-placeholder"),
        Style {
            width: Dimension::Percent(100.0),
            font_size: 15.0,
            color: Color::from_hex("#1a2836"),
            background: Color::from_hex("#f4f7fb"),
            border_radius: 10.0,
            padding: Edges::xy(12.0, if coarse { 12.0 } else { 8.0 }),
            min_height: Dimension::Px(if coarse { 48.0 } else { 36.0 }),
            ..Style::default()
        },
    )
}

fn sign_message_field() -> Component {
    let coarse = state::pointer_is_coarse();
    let mut style = Style {
        width: Dimension::Percent(100.0),
        font_size: 15.0,
        color: Color::from_hex("#1a2836"),
        background: Color::from_hex("#f4f7fb"),
        border_radius: 10.0,
        padding: Edges::xy(12.0, if coarse { 12.0 } else { 8.0 }),
        min_height: Dimension::Px(if coarse { 48.0 } else { 36.0 }),
        ..Style::default()
    };
    style.custom_properties = Some(HashMap::from([
        (
            "--w3cos-intent-submit".to_string(),
            OP_SIGN_PROPOSE.to_string(),
        ),
        ("--w3cos-ime".to_string(), "sign".to_string()),
    ]));
    note_preedit(&mut style, "sign");
    Component::text_input(
        state::get_signal_text(SIGN_MESSAGE_SIGNAL),
        locale::t("sign-message-placeholder"),
        style,
    )
}

fn settings_agent_page() -> Vec<Component> {
    let bridge = std::env::var("W3COS_AI_PERMISSIONS").unwrap_or_default();
    let bridge = match bridge.trim() {
        "" | "observer" => locale::t("perm-observer"),
        "interactive" => locale::t("perm-interactive"),
        "system" => locale::t("perm-system"),
        _ => locale::t("perm-unknown"),
    };
    let shared = state::get_signal(CONTEXT_SIGNAL) != 0;
    let mut rows = settings_heading(
        &locale::t("settings-agent-title"),
        &locale::t("settings-agent-detail"),
    );
    rows.push(settings_switch(
        &locale::t("settings-context-switch"),
        shared,
        if shared { OP_REVOKE } else { OP_RESTORE },
    ));
    rows.push(Component::text(
        locale::t("settings-context-note"),
        Style {
            font_size: 13.0,
            color: Color::from_hex("#d5dee8"),
            ..Style::default()
        },
    ));
    rows.push(settings_row(&locale::t("perm-bridge"), &bridge));
    rows.push(settings_row(
        &locale::t("perm-shell"),
        &locale::t("perm-shell-value"),
    ));
    rows.push(settings_row(
        &locale::t("perm-boot"),
        &locale::t("perm-boot-value"),
    ));
    rows.push(settings_row(
        &locale::t("perm-standing"),
        &locale::t("perm-standing-value"),
    ));
    rows.push(settings_link(locale::t("settings-l2-open"), OP_SETTINGS_L2));
    rows
}

fn settings_l2_page() -> Vec<Component> {
    let env_flag = std::env::var("W3COS_L2").unwrap_or_default();
    let env_flag = if env_flag.trim().is_empty() {
        locale::t("l2-unset")
    } else {
        env_flag
    };
    let model = std::env::var("W3COS_L2_MODEL").unwrap_or_default();
    let model = if model.trim().is_empty() {
        locale::t("l2-model-default")
    } else {
        model
    };
    let endpoint = std::env::var("W3COS_L2_URL").unwrap_or_default();
    let endpoint = if endpoint.trim().is_empty() {
        locale::t("l2-endpoint-default")
    } else {
        endpoint
    };
    let on = session_l2_on();
    let current = if on {
        locale::t("on")
    } else {
        locale::t("off")
    };
    let mut rows = settings_heading(&locale::t("l2-title"), &locale::t("l2-detail"));
    rows.push(settings_row(&locale::t("l2-current"), &current));
    rows.push(settings_row(&locale::t("l2-env"), &env_flag));
    rows.push(settings_row(&locale::t("l2-model"), &model));
    rows.push(settings_row(&locale::t("l2-endpoint"), &endpoint));
    rows.push(settings_row(
        &locale::t("infer-backend"),
        &infer_backend_label(),
    ));
    let in_process = w3cos_ai_bridge::infer::active_backend()
        == w3cos_ai_bridge::infer::InferBackendKind::InProcess;
    rows.push(settings_switch(
        &locale::t("infer-backend-toggle"),
        in_process,
        OP_INFER_BACKEND,
    ));
    rows.push(Component::text(
        locale::t("infer-backend-detail"),
        Style {
            font_size: 13.0,
            color: Color::from_hex("#d5dee8"),
            ..Style::default()
        },
    ));
    if w3cos_ai_bridge::infer::requested_backend()
        == w3cos_ai_bridge::infer::InferBackendKind::InProcess
        && !w3cos_ai_bridge::infer::in_process_compiled()
    {
        rows.push(Component::text(
            locale::t("infer-backend-unavailable"),
            Style {
                font_size: 13.0,
                color: Color::from_hex("#ffe08a"),
                ..Style::default()
            },
        ));
    }
    rows.push(chrome_button(
        if on {
            locale::t("l2-turn-off")
        } else {
            locale::t("l2-turn-on")
        },
        OP_L2_TOGGLE,
        if on { "#3d5570" } else { "#1d4e89" },
    ));
    rows
}

fn infer_backend_label() -> String {
    if w3cos_ai_bridge::infer::active_backend()
        == w3cos_ai_bridge::infer::InferBackendKind::InProcess
    {
        locale::t("infer-backend-in-process")
    } else {
        locale::t("infer-backend-inferd")
    }
}

fn settings_row(label: &str, value: &str) -> Component {
    Component::row(
        Style {
            padding: Edges::all(14.0),
            background: Color::from_hex("#31475e"),
            border_radius: 8.0,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![
            Component::text(
                label,
                Style {
                    font_size: 14.0,
                    color: Color::from_hex("#f4f7fb"),
                    ..Style::default()
                },
            ),
            Component::text(
                value,
                Style {
                    font_size: 14.0,
                    color: Color::from_hex("#ffe08a"),
                    ..Style::default()
                },
            ),
        ],
    )
}

// ==========================================================================
// Chrome: context header, intent bar, and the agent task center.
// Matching runs in the invoke hook, not while this tree is built.
// ==========================================================================

fn ink() -> Color {
    Color::from_hex("#f4f7fb")
}

fn muted_ink() -> Color {
    Color::from_hex("#d5dee8")
}

fn build_task_center() -> Component {
    let phase = state::get_signal(PHASE_SIGNAL);
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let (status, status_color) = match phase {
        1 => (locale::t("phase-confirm"), "#ffe08a"),
        2 => (locale::t("phase-done"), "#b7f0d8"),
        3 => (locale::t("phase-planning"), "#9eb6ff"),
        _ => (locale::t("phase-idle"), "#d5dee8"),
    };
    let mut body = vec![
        Component::row(
            Style {
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                gap: 8.0,
                ..Style::default()
            },
            vec![
                Component::text(
                    locale::t("tasks-title"),
                    Style {
                        font_size: 16.0,
                        font_weight: 700,
                        color: ink(),
                        flex_grow: 1.0,
                        ..Style::default()
                    },
                ),
                chrome_button(locale::t("tasks-hide"), OP_TOGGLE_TASKS, "#3d5570"),
            ],
        ),
        Component::text(
            planner_subtitle(),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        Component::text(
            status,
            Style {
                font_size: 13.0,
                font_weight: 700,
                color: Color::from_hex(status_color),
                ..Style::default()
            },
        ),
        Component::text(
            {
                let subject_owned = state::get_signal_text(SUBJECT_SIGNAL);
                let subject_label = if subject_owned.is_empty() {
                    LocalIdentityStub::SUBJECT.to_string()
                } else {
                    subject_owned
                };
                {
                    let state_label = if unlocked {
                        locale::t("unlocked")
                    } else {
                        locale::t("locked")
                    };
                    let line = if state::get_signal_text(ENROLL_SIGNAL) == "verified" {
                        "identity-line-passkey"
                    } else {
                        "identity-line"
                    };
                    locale::t_args(
                        line,
                        &[("subject", &subject_label), ("state", &state_label)],
                    )
                }
            },
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
    ];
    if !unlocked {
        body.push(Component::text(
            locale::t("locked-hint"),
            Style {
                font_size: 13.0,
                font_weight: 700,
                color: Color::from_hex("#ffe08a"),
                ..Style::default()
            },
        ));
    }
    if unlocked {
        body.push(chrome_button(locale::t("lock"), OP_LOCK, "#3d5570"));
    }
    let steps = state::get_signal_text(STEPS_SIGNAL);
    if !steps.is_empty() {
        body.push(Component::text(
            locale::t("steps"),
            Style {
                font_size: 14.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ));
        for line in steps
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            body.push(Component::text(
                line,
                Style {
                    font_size: 12.0,
                    color: ink(),
                    ..Style::default()
                },
            ));
        }
    }
    body.push(Component::text(
        locale::t("receipts"),
        Style {
            font_size: 14.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    ));
    body.push(chrome_button(
        locale::t("ledger"),
        OP_TOGGLE_RECEIPTS,
        "#3d5570",
    ));
    let log = state::get_signal_text(RECEIPT_SIGNAL);
    if log.is_empty() || log == "No receipts yet." {
        body.push(Component::text(
            locale::t("no-receipts"),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for line in log.lines().take(8) {
            body.push(Component::text(
                present_receipt(line),
                Style {
                    font_size: 12.0,
                    color: ink(),
                    ..Style::default()
                },
            ));
        }
    }
    if phase == 1 || phase == 3 {
        body.push(chrome_button(locale::t("stop"), OP_CANCEL, "#a33b45"));
    }
    Component::column(
        Style {
            width: Dimension::Px(300.0),
            flex_shrink: 0.0,
            min_height: Dimension::Px(0.0),
            overflow: Overflow::Auto,
            padding: Edges::all(14.0),
            gap: 8.0,
            background: Color::from_hex("#1b2c3e"),
            ..Style::default()
        },
        body,
    )
}

fn planner_subtitle() -> String {
    // L2 off never claims a model or a background thread. The on-copy names
    // the fixture or the path, and says inference has not been started.
    if !session_l2_on() {
        return if state::get_signal_text(STORAGE_SIGNAL) == "encrypted-file" {
            locale::t("planner-encrypted")
        } else {
            locale::t("planner-memory")
        };
    }
    match std::env::var("W3COS_L2").ok().as_deref().map(str::trim) {
        Some("script") => locale::t("planner-script"),
        Some("1") | Some("true") => {
            let model = std::env::var("W3COS_L2_MODEL").unwrap_or_else(|_| "local".into());
            locale::t_args("planner-model", &[("model", &model)])
        }
        _ => locale::t("planner-memory"),
    }
}

#[derive(Clone, Copy)]
enum SheetKind {
    Ghost,
    Primary,
    Danger,
    Warm,
}

fn action_button(label: impl Into<String>, code: u8, kind: SheetKind) -> Component {
    let (background, color, border) = match kind {
        SheetKind::Ghost => (Color::TRANSPARENT, ink_token(), Some(line_strong())),
        SheetKind::Primary => (Color::from_hex("#0c6d4c"), Color::from_hex("#e9fff5"), None),
        SheetKind::Warm => (Color::from_hex("#9a5410"), Color::from_hex("#fff4e5"), None),
        SheetKind::Danger => (
            Color::TRANSPARENT,
            Color::from_hex("#ffd5d4"),
            Some(Color::rgba(224, 122, 120, 115)),
        ),
    };
    let height = touch_min();
    Component::button_with_click(
        label,
        Style {
            background,
            color,
            border_width: if border.is_some() { 1.0 } else { 0.0 },
            border_color: border.unwrap_or(Color::TRANSPARENT),
            border_radius: 10.0,
            font_size: 14.0,
            font_weight: 600,
            padding: Edges::xy(14.0, 0.0),
            height: Dimension::Px(height),
            min_height: Dimension::Px(height),
            box_sizing: BoxSizing::BorderBox,
            ..Style::default()
        },
        invoke(code),
    )
}

fn chrome_button(label: impl Into<String>, code: u8, background: &str) -> Component {
    let action = if code == OP_PROPOSE {
        EventAction::Invoke {
            code,
            input_placeholder: locale::t("intent-placeholder"),
        }
    } else {
        invoke(code)
    };
    let height = touch_min();
    Component::button_with_click(
        label,
        Style {
            background: Color::from_hex(background),
            border_radius: 10.0,
            font_size: 13.0,
            font_weight: 650,
            color: Color::WHITE,
            padding: Edges::xy(12.0, 0.0),
            height: Dimension::Px(height),
            min_height: Dimension::Px(height),
            box_sizing: BoxSizing::BorderBox,
            ..Style::default()
        },
        action,
    )
}

// ==========================================================================
// Browser App
// ==========================================================================

fn build_browser_app() -> Component {
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#0e0e18"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(locale::t("app-browser"), "#f39c12", 5),
            // URL bar
            Component::row(
                Style {
                    height: Dimension::Px(44.0),
                    background: Color::from_hex("#14142a"),
                    padding: Edges::xy(12.0, 6.0),
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    Component::button(
                        "<",
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#808090"),
                            background: Color::from_hex("#1e1e38"),
                            border_radius: 6.0,
                            padding: Edges::xy(10.0, 6.0),
                            ..Style::default()
                        },
                    ),
                    Component::button(
                        ">",
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#808090"),
                            background: Color::from_hex("#1e1e38"),
                            border_radius: 6.0,
                            padding: Edges::xy(10.0, 6.0),
                            ..Style::default()
                        },
                    ),
                    Component::button(
                        "R",
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#808090"),
                            background: Color::from_hex("#1e1e38"),
                            border_radius: 6.0,
                            padding: Edges::xy(10.0, 6.0),
                            ..Style::default()
                        },
                    ),
                    Component::row(
                        Style {
                            flex_grow: 1.0,
                            background: Color::from_hex("#1e1e38"),
                            border_radius: 8.0,
                            padding: Edges::xy(12.0, 8.0),
                            align_items: AlignItems::Center,
                            gap: 8.0,
                            ..Style::default()
                        },
                        vec![Component::text(
                            "https://w3cos.dev",
                            Style {
                                font_size: 13.0,
                                color: Color::from_hex("#a0a0c0"),
                                ..Style::default()
                            },
                        )],
                    ),
                    Component::button(
                        "+",
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#808090"),
                            background: Color::from_hex("#1e1e38"),
                            border_radius: 6.0,
                            padding: Edges::xy(10.0, 6.0),
                            ..Style::default()
                        },
                    ),
                ],
            ),
            // Tab bar
            Component::row(
                Style {
                    height: Dimension::Px(36.0),
                    background: Color::from_hex("#12121f"),
                    padding: Edges::xy(8.0, 4.0),
                    gap: 4.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    browser_tab("W3C OS", true),
                    browser_tab(&locale::t("browser-tab-docs"), false),
                    browser_tab("GitHub", false),
                ],
            ),
            // Page content
            Component::column(
                Style {
                    flex_grow: 1.0,
                    padding: Edges::all(32.0),
                    gap: 24.0,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    background: Color::from_hex("#0a0a14"),
                    ..Style::default()
                },
                vec![
                    Component::text(
                        "W3C OS",
                        Style {
                            font_size: 48.0,
                            color: Color::from_hex("#1c8f62"),
                            font_weight: 700,
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        locale::t("browser-tagline"),
                        Style {
                            font_size: 18.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::column(
                        Style {
                            padding: Edges::all(24.0),
                            background: Color::from_hex("#16162a"),
                            border_radius: 12.0,
                            gap: 16.0,
                            max_width: Dimension::Px(600.0),
                            ..Style::default()
                        },
                        vec![
                            Component::text(
                                locale::t("browser-welcome"),
                                Style {
                                    font_size: 16.0,
                                    color: Color::from_hex("#f0f0ff"),
                                    font_weight: 600,
                                    ..Style::default()
                                },
                            ),
                            Component::text(
                                locale::t("browser-body"),
                                Style {
                                    font_size: 13.0,
                                    color: Color::from_hex("#808090"),
                                    ..Style::default()
                                },
                            ),
                            Component::row(
                                Style {
                                    gap: 12.0,
                                    ..Style::default()
                                },
                                vec![
                                    Component::button(
                                        locale::t("browser-start"),
                                        Style {
                                            background: Color::from_hex("#1c8f62"),
                                            border_radius: 8.0,
                                            font_size: 14.0,
                                            color: Color::WHITE,
                                            padding: Edges::xy(16.0, 10.0),
                                            ..Style::default()
                                        },
                                    ),
                                    Component::button(
                                        locale::t("browser-docs"),
                                        Style {
                                            background: Color::from_hex("#1e1e38"),
                                            border_radius: 8.0,
                                            font_size: 14.0,
                                            color: Color::from_hex("#a0a0c0"),
                                            padding: Edges::xy(16.0, 10.0),
                                            ..Style::default()
                                        },
                                    ),
                                ],
                            ),
                        ],
                    ),
                ],
            ),
        ],
    )
}

fn browser_tab(title: &str, active: bool) -> Component {
    let bg = if active { "#1e1e38" } else { "#12121f" };
    let color = if active { "#e0e0f0" } else { "#606080" };
    Component::row(
        Style {
            padding: Edges::xy(12.0, 6.0),
            background: Color::from_hex(bg),
            border_radius: 6.0,
            gap: 8.0,
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![
            Component::text(
                title,
                Style {
                    font_size: 12.0,
                    color: Color::from_hex(color),
                    ..Style::default()
                },
            ),
            Component::text(
                "x",
                Style {
                    font_size: 10.0,
                    color: Color::from_hex("#505070"),
                    ..Style::default()
                },
            ),
        ],
    )
}

// ==========================================================================
// Editor App
// ==========================================================================

fn build_editor_app() -> Component {
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#1e1e2e"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(locale::t("app-editor"), "#1abc9c", 6),
            // Editor toolbar
            Component::row(
                Style {
                    height: Dimension::Px(36.0),
                    background: Color::from_hex("#181828"),
                    padding: Edges::xy(8.0, 4.0),
                    gap: 16.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        locale::t("editor-file"),
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        locale::t("editor-edit"),
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        locale::t("editor-view"),
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        locale::t("app-terminal"),
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        locale::t("editor-help"),
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                ],
            ),
            // Tab bar
            Component::row(
                Style {
                    height: Dimension::Px(34.0),
                    background: Color::from_hex("#14142a"),
                    padding: Edges::xy(4.0, 0.0),
                    gap: 0.0,
                    align_items: AlignItems::FlexEnd,
                    ..Style::default()
                },
                vec![
                    editor_tab("app.tsx", true),
                    editor_tab("styles.css", false),
                    editor_tab("Cargo.toml", false),
                ],
            ),
            Component::row(
                Style {
                    flex_grow: 1.0,
                    gap: 0.0,
                    ..Style::default()
                },
                vec![
                    // Explorer sidebar
                    Component::column(
                        Style {
                            width: Dimension::Px(220.0),
                            background: Color::from_hex("#16162a"),
                            padding: Edges::all(8.0),
                            gap: 2.0,
                            ..Style::default()
                        },
                        vec![
                            Component::text(
                                locale::t("editor-explorer"),
                                Style {
                                    font_size: 11.0,
                                    color: Color::from_hex("#505070"),
                                    font_weight: 700,
                                    padding: Edges::xy(8.0, 6.0),
                                    ..Style::default()
                                },
                            ),
                            explorer_item("src", 0, true),
                            explorer_item("app.tsx", 1, false),
                            explorer_item("main.rs", 1, false),
                            explorer_item("styles", 0, true),
                            explorer_item("theme.css", 1, false),
                            explorer_item("Cargo.toml", 0, false),
                            explorer_item("README.md", 0, false),
                        ],
                    ),
                    // Line numbers + code
                    Component::row(
                        Style {
                            flex_grow: 1.0,
                            gap: 0.0,
                            ..Style::default()
                        },
                        vec![
                            // Gutter
                            Component::column(
                                Style {
                                    width: Dimension::Px(48.0),
                                    background: Color::from_hex("#1a1a2e"),
                                    padding: Edges {
                                        top: 8.0.into(),
                                        right: 8.0.into(),
                                        bottom: 8.0.into(),
                                        left: 0.0.into(),
                                    },
                                    align_items: AlignItems::FlexEnd,
                                    gap: 0.0,
                                    ..Style::default()
                                },
                                vec![
                                    gutter_line(1),
                                    gutter_line(2),
                                    gutter_line(3),
                                    gutter_line(4),
                                    gutter_line(5),
                                    gutter_line(6),
                                    gutter_line(7),
                                    gutter_line(8),
                                    gutter_line(9),
                                    gutter_line(10),
                                    gutter_line(11),
                                    gutter_line(12),
                                    gutter_line(13),
                                    gutter_line(14),
                                    gutter_line(15),
                                ],
                            ),
                            // Code area
                            Component::column(
                                Style {
                                    flex_grow: 1.0,
                                    padding: Edges::all(8.0),
                                    gap: 0.0,
                                    overflow: Overflow::Scroll,
                                    ..Style::default()
                                },
                                vec![
                                    code_line(
                                        "import",
                                        " { useState } ",
                                        "from",
                                        " 'react';",
                                        &["#c678dd", "#d0d0e0", "#c678dd", "#98c379"],
                                    ),
                                    code_line(
                                        "import",
                                        " './styles.css';",
                                        "",
                                        "",
                                        &["#c678dd", "#98c379", "", ""],
                                    ),
                                    code_line("", "", "", "", &[]),
                                    code_line(
                                        "export default function ",
                                        "App",
                                        "() {",
                                        "",
                                        &["#c678dd", "#61afef", "#d0d0e0", ""],
                                    ),
                                    code_line(
                                        "  const ",
                                        "[count, setCount]",
                                        " = ",
                                        "useState(0);",
                                        &["#c678dd", "#e06c75", "#d0d0e0", "#61afef"],
                                    ),
                                    code_line("", "", "", "", &[]),
                                    code_line(
                                        "  return ",
                                        "(",
                                        "",
                                        "",
                                        &["#c678dd", "#d0d0e0", "", ""],
                                    ),
                                    code_line(
                                        "    <",
                                        "div",
                                        " className=\"container\">",
                                        "",
                                        &["#d0d0e0", "#e06c75", "#d0d0e0", ""],
                                    ),
                                    code_line(
                                        "      <",
                                        "h1",
                                        ">W3C OS App</",
                                        "h1>",
                                        &["#d0d0e0", "#e06c75", "#d0d0e0", "#e06c75"],
                                    ),
                                    code_line(
                                        "      <",
                                        "p",
                                        ">Count: {count}</",
                                        "p>",
                                        &["#d0d0e0", "#e06c75", "#d0d0e0", "#e06c75"],
                                    ),
                                    code_line(
                                        "      <",
                                        "button",
                                        " onClick={() => setCount(c => c + 1)}>",
                                        "",
                                        &["#d0d0e0", "#e06c75", "#d0d0e0", ""],
                                    ),
                                    code_line(
                                        "        Increment",
                                        "",
                                        "",
                                        "",
                                        &["#d0d0e0", "", "", ""],
                                    ),
                                    code_line(
                                        "      </",
                                        "button",
                                        ">",
                                        "",
                                        &["#d0d0e0", "#e06c75", "#d0d0e0", ""],
                                    ),
                                    code_line(
                                        "    </",
                                        "div",
                                        ">",
                                        "",
                                        &["#d0d0e0", "#e06c75", "#d0d0e0", ""],
                                    ),
                                    code_line("  );", "", "", "", &["#d0d0e0", "", "", ""]),
                                ],
                            ),
                        ],
                    ),
                ],
            ),
            // Status bar
            Component::row(
                Style {
                    height: Dimension::Px(24.0),
                    background: Color::from_hex("#1c8f62"),
                    padding: Edges::xy(12.0, 4.0),
                    gap: 16.0,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        "TypeScript React",
                        Style {
                            font_size: 11.0,
                            color: Color::WHITE,
                            ..Style::default()
                        },
                    ),
                    Component::row(
                        Style {
                            gap: 16.0,
                            ..Style::default()
                        },
                        vec![
                            Component::text(
                                "Ln 4, Col 12",
                                Style {
                                    font_size: 11.0,
                                    color: Color::WHITE,
                                    ..Style::default()
                                },
                            ),
                            Component::text(
                                "UTF-8",
                                Style {
                                    font_size: 11.0,
                                    color: Color::WHITE,
                                    ..Style::default()
                                },
                            ),
                            Component::text(
                                "Spaces: 2",
                                Style {
                                    font_size: 11.0,
                                    color: Color::WHITE,
                                    ..Style::default()
                                },
                            ),
                        ],
                    ),
                ],
            ),
        ],
    )
}

fn editor_tab(name: &str, active: bool) -> Component {
    let bg = if active { "#1e1e2e" } else { "#14142a" };
    let color = if active { "#e0e0f0" } else { "#606080" };
    let border_color = if active { "#1c8f62" } else { "#14142a" };
    Component::column(
        Style {
            padding: Edges::xy(14.0, 8.0),
            background: Color::from_hex(bg),
            border_width: 2.0,
            border_color: Color::from_hex(border_color),
            ..Style::default()
        },
        vec![Component::text(
            name,
            Style {
                font_size: 12.0,
                color: Color::from_hex(color),
                ..Style::default()
            },
        )],
    )
}

fn explorer_item(name: &str, indent: u8, is_dir: bool) -> Component {
    let icon = if is_dir { "▸ " } else { "  " };
    let color = if is_dir { "#e0e0f0" } else { "#a0a0c0" };
    let pad_left = 8.0 + (indent as f32 * 12.0);
    Component::row(
        Style {
            padding: Edges {
                top: 3.0.into(),
                right: 8.0.into(),
                bottom: 3.0.into(),
                left: pad_left.into(),
            },
            gap: 4.0,
            align_items: AlignItems::Center,
            border_radius: 4.0,
            ..Style::default()
        },
        vec![
            Component::text(
                icon,
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#1c8f62"),
                    ..Style::default()
                },
            ),
            Component::text(
                name,
                Style {
                    font_size: 12.0,
                    color: Color::from_hex(color),
                    ..Style::default()
                },
            ),
        ],
    )
}

fn gutter_line(n: u32) -> Component {
    Component::text(
        &format!("{n}"),
        Style {
            font_size: 13.0,
            color: Color::from_hex("#505070"),
            height: Dimension::Px(20.0),
            ..Style::default()
        },
    )
}

fn code_line(a: &str, b: &str, c: &str, d: &str, colors: &[&str]) -> Component {
    let mut parts = Vec::new();
    let segments = [(a, 0), (b, 1), (c, 2), (d, 3)];
    for (text, idx) in &segments {
        if !text.is_empty() {
            let color = colors.get(*idx).copied().unwrap_or("#d0d0e0");
            if !color.is_empty() {
                parts.push(Component::text(
                    *text,
                    Style {
                        font_size: 13.0,
                        color: Color::from_hex(color),
                        ..Style::default()
                    },
                ));
            }
        }
    }
    if parts.is_empty() {
        Component::text(
            " ",
            Style {
                font_size: 13.0,
                height: Dimension::Px(20.0),
                ..Style::default()
            },
        )
    } else {
        Component::row(
            Style {
                gap: 0.0,
                height: Dimension::Px(20.0),
                ..Style::default()
            },
            parts,
        )
    }
}

// ==========================================================================
// Shared: title bar + taskbar
// ==========================================================================

fn window_button(
    glyph: &str,
    name: impl Into<String>,
    code: u8,
    stroke: &str,
    app_id: i64,
) -> Component {
    // Desktop title buttons stay 28px. Phone and tablet use 48px.
    let side = gesture::coarse_target(state::pointer_is_coarse());
    let mut button = glyph_face(
        glyph,
        name,
        code,
        Color::TRANSPARENT,
        stroke,
        side,
        8.0,
        false,
    );
    button.style.custom_properties = Some(win_props(app_id, None));
    button
}

fn app_titlebar(title: impl Into<String>, accent: &str, app_id: i64) -> Component {
    let title = title.into();
    let floating = posture() == Posture::Desktop;
    let (max_glyph, max_label) = if windows::is_maximized(app_id) {
        ("restore", locale::t("win-restore"))
    } else {
        ("maximize", locale::t("win-maximize"))
    };
    let mut controls = vec![window_button(
        "minus",
        locale::t("win-minimize"),
        OP_MINIMIZE,
        "#c5d0d8",
        app_id,
    )];
    // Phone and tablet already fill the surface. A maximize control there
    // would only flip stored geometry and leave the screen unchanged.
    if floating {
        controls.push(window_button(
            max_glyph,
            max_label,
            OP_MAXIMIZE,
            "#c5d0d8",
            app_id,
        ));
    }
    controls.push(window_button(
        "close",
        locale::t("win-close"),
        OP_CLOSE,
        "#e94560",
        app_id,
    ));
    let bar_height = if state::pointer_is_coarse() {
        56.0
    } else {
        40.0
    };
    let mut bar = Component::row(
        Style {
            height: Dimension::Px(bar_height),
            flex_shrink: 0.0,
            background: Color::from_hex("#2a3e52"),
            padding: Edges::xy(16.0, 8.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..Style::default()
        },
        vec![
            Component::row(
                Style {
                    gap: 10.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    Component::column(
                        Style {
                            width: Dimension::Px(12.0),
                            height: Dimension::Px(12.0),
                            background: Color::from_hex(accent),
                            border_radius: 6.0,
                            ..Style::default()
                        },
                        vec![],
                    ),
                    Component::text(
                        title,
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#e0e0f0"),
                            font_weight: 600,
                            ..Style::default()
                        },
                    ),
                ],
            ),
            Component::row(
                Style {
                    gap: 4.0,
                    ..Style::default()
                },
                controls,
            ),
        ],
    );
    bar.kind = ComponentKind::Button {
        label: String::new(),
    };
    bar.on_click = invoke(focus_code(app_id));
    bar.style.custom_properties = Some(win_props(app_id, Some("move")));
    bar
}

struct LaunchApp {
    id: i64,
    letter: &'static str,
    glyph: &'static str,
    color: &'static str,
}

const LAUNCH_APPS: [LaunchApp; 6] = [
    LaunchApp {
        id: 0,
        letter: "D",
        glyph: "grid",
        color: "#1c8f62",
    },
    LaunchApp {
        id: 1,
        letter: "F",
        glyph: "files",
        color: "#3498db",
    },
    LaunchApp {
        id: 2,
        letter: "T",
        glyph: "terminal",
        color: "#2ecc71",
    },
    LaunchApp {
        id: 3,
        letter: "S",
        glyph: "settings",
        color: "#d4a017",
    },
    LaunchApp {
        id: 5,
        letter: "B",
        glyph: "browser",
        color: "#f39c12",
    },
    LaunchApp {
        id: 6,
        letter: "E",
        glyph: "editor",
        color: "#1abc9c",
    },
];

fn build_launcher() -> Component {
    let query = state::get_signal_text(LAUNCHER_QUERY_SIGNAL);
    let mut search_style = Style {
        flex_grow: 1.0,
        font_size: 14.0,
        color: Color::from_hex("#1a2836"),
        background: Color::from_hex("#f4f7fb"),
        border_radius: 8.0,
        padding: Edges::xy(12.0, 8.0),
        ..Style::default()
    };
    search_style.custom_properties = Some(HashMap::from([
        (
            "--w3cos-intent-submit".to_string(),
            OP_LAUNCHER_SEARCH.to_string(),
        ),
        ("--w3cos-ime".to_string(), "launcher".to_string()),
    ]));
    note_preedit(&mut search_style, "launcher");
    let needle = query.trim().to_lowercase();
    let mut tiles = Vec::new();
    for app in LAUNCH_APPS {
        let hay = format!(
            "{} {} {}",
            locale::t_in("zh-CN", app_key(app.id)),
            locale::t_in("en", app_key(app.id)),
            app.letter
        )
        .to_lowercase();
        if !needle.is_empty() && !hay.contains(&needle) {
            continue;
        }
        tiles.push(launcher_tile(app));
    }
    if tiles.is_empty() {
        tiles.push(Component::text(
            locale::t("launcher-empty"),
            Style {
                font_size: 13.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        ));
    }
    let query_label = if query.trim().is_empty() {
        locale::t("launcher-all")
    } else {
        let shown = query.trim();
        locale::t_args("launcher-query", &[("query", shown)])
    };
    let mut children = vec![
        Component::row(
            Style {
                gap: 8.0,
                align_items: AlignItems::Center,
                ..Style::default()
            },
            vec![
                Component::text_input(
                    query.clone(),
                    locale::t("launcher-placeholder"),
                    search_style,
                ),
                Component::button_with_click(
                    locale::t("launcher-search"),
                    Style {
                        background: Color::from_hex("#1c8f62"),
                        border_radius: 8.0,
                        font_size: 13.0,
                        font_weight: 700,
                        color: Color::WHITE,
                        padding: Edges::xy(12.0, 8.0),
                        ..Style::default()
                    },
                    EventAction::Invoke {
                        code: OP_LAUNCHER_SEARCH,
                        input_placeholder: locale::t("launcher-placeholder"),
                    },
                ),
            ],
        ),
        Component::text(
            query_label,
            Style {
                font_size: 12.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        ),
        Component::row(
            Style {
                gap: 8.0,
                flex_wrap: FlexWrap::Wrap,
                ..Style::default()
            },
            tiles,
        ),
    ];
    if std::env::var_os("W3COS_SHARED_LAUNCHER").is_some() {
        children.push(build_shared_launcher());
    }
    let (width, height) = state::shell_viewport();
    let panel_w = 440.0_f32.min((width - 32.0).max(240.0));
    Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(16.0),
            top: Dimension::Px((height - 80.0 - 16.0 - 280.0).max(16.0)),
            width: Dimension::Px(panel_w),
            max_height: Dimension::Px(320.0),
            overflow: Overflow::Auto,
            background: Color::from_hex("#1b2c3e"),
            padding: Edges::all(12.0),
            gap: 8.0,
            border_radius: 14.0,
            z_index: CHROME_Z + 1,
            ..Style::default()
        },
        children,
    )
}

fn launcher_tile(app: LaunchApp) -> Component {
    let current = state::get_signal(APP_SIGNAL) == app.id;
    let open = state::get_signal(OPEN_MASK_SIGNAL) & app_bit(app.id) != 0;
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL) & app_bit(app.id) != 0;
    let status = if current {
        locale::t("launcher-front")
    } else if minimized {
        locale::t("launcher-min")
    } else if open {
        locale::t("launcher-open")
    } else {
        String::new()
    };
    let name = app_label(app.id);
    let label = if status.is_empty() {
        name
    } else {
        format!("{name} · {status}")
    };
    let mut tile = Component::column(
        Style {
            width: Dimension::Px(108.0),
            align_items: AlignItems::Center,
            gap: 6.0,
            background: Color::from_hex(app.color),
            border_radius: 12.0,
            padding: Edges::all(8.0),
            ..Style::default()
        },
        vec![
            Component::svg_document(
                glyph_svg(app.glyph, "white"),
                24,
                24,
                Style {
                    width: Dimension::Px(28.0),
                    height: Dimension::Px(28.0),
                    ..Style::default()
                },
            ),
            Component::text(
                label,
                Style {
                    font_size: 12.0,
                    font_weight: 700,
                    color: Color::WHITE,
                    ..Style::default()
                },
            ),
        ],
    );
    tile.kind = ComponentKind::Button {
        label: String::new(),
    };
    tile.on_click = invoke(focus_code(app.id));
    tile
}

fn build_toast(text: &str) -> Component {
    let (width, _) = state::shell_viewport();
    let toast_w = 480.0_f32.min((width - 32.0).max(200.0));
    Component::row(
        Style {
            position: Position::Absolute,
            left: Dimension::Px(((width - toast_w) / 2.0).max(8.0)),
            top: Dimension::Px(16.0),
            width: Dimension::Px(toast_w),
            flex_shrink: 0.0,
            padding: Edges::xy(16.0, 8.0),
            gap: 8.0,
            background: Color::from_hex("#1d4e89"),
            align_items: AlignItems::Center,
            border_radius: 12.0,
            z_index: CHROME_Z + 1,
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t("toast-title"),
                Style {
                    font_size: 13.0,
                    font_weight: 700,
                    color: Color::WHITE,
                    ..Style::default()
                },
            ),
            Component::text(
                text,
                Style {
                    font_size: 13.0,
                    color: Color::from_hex("#f4f7fb"),
                    flex_grow: 1.0,
                    ..Style::default()
                },
            ),
            chrome_button(locale::t("toast-dismiss"), OP_DISMISS_TOAST, "#3d5570"),
        ],
    )
}

fn receipt_query_field() -> Component {
    let mut style = Style {
        font_size: 13.0,
        color: ink_token(),
        background: Color::rgba(244, 241, 234, 18),
        border_radius: 10.0,
        border_width: 1.0,
        border_color: Color::rgba(244, 241, 234, 31),
        padding: Edges::xy(10.0, 6.0),
        min_height: Dimension::Px(if state::pointer_is_coarse() {
            48.0
        } else {
            32.0
        }),
        ..Style::default()
    };
    style.custom_properties = Some(HashMap::from([
        (
            "--w3cos-intent-submit".to_string(),
            OP_RECEIPT_QUERY.to_string(),
        ),
        ("--w3cos-ime".to_string(), "text".to_string()),
    ]));
    note_preedit(&mut style, "text");
    Component::text_input(
        state::get_signal_text(RECEIPT_QUERY_SIGNAL),
        locale::t("receipt-query"),
        style,
    )
}

fn filter_chip(key: &str, code: u8, selected: bool) -> Component {
    let height = if state::pointer_is_coarse() {
        48.0
    } else {
        28.0
    };
    Component::button_with_click(
        locale::t(key),
        Style {
            height: Dimension::Px(height),
            min_height: Dimension::Px(height),
            box_sizing: BoxSizing::BorderBox,
            padding: Edges::xy(10.0, 0.0),
            border_radius: 999.0,
            border_width: if selected { 0.0 } else { 1.0 },
            border_color: Color::rgba(244, 241, 234, 31),
            background: if selected {
                Color::rgba(244, 241, 234, 26)
            } else {
                Color::TRANSPARENT
            },
            color: if selected { ink_token() } else { muted_token() },
            font_size: 12.0,
            ..Style::default()
        },
        invoke(code),
    )
}

fn receipt_row(line: &str) -> Component {
    let parts = parse_receipt(line);
    let (pill_bg, pill_fg) = decision_colors(&parts.decision);
    let clock = format_receipt_clock(&parts.recorded_at);
    let clock = if clock.is_empty() {
        "—".to_string()
    } else {
        clock
    };
    let summary = if !parts.utterance.is_empty() {
        parts.utterance.clone()
    } else if !parts.detail.is_empty() {
        parts.detail.clone()
    } else {
        parts.command.clone()
    };
    let layer = layer_label(&parts.tier);
    let actor = if parts.actor.is_empty() {
        String::new()
    } else {
        truncate_middle(&parts.actor, 22)
    };
    let meta = if actor.is_empty() {
        locale::t_args("receipt-meta", &[("layer", &layer), ("id", &parts.id)])
    } else {
        locale::t_args(
            "receipt-meta-actor",
            &[("layer", &layer), ("id", &parts.id), ("actor", &actor)],
        )
    };
    let show_result = !parts.detail.is_empty() && parts.detail != summary;
    let mut lines = vec![
        Component::row(
            Style {
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                gap: 8.0,
                ..Style::default()
            },
            vec![
                Component::text(
                    clock,
                    Style {
                        font_size: 12.0,
                        color: faint_token(),
                        flex_shrink: 0.0,
                        ..Style::default()
                    },
                ),
                Component::text(
                    decision_label(&parts.decision),
                    Style {
                        font_size: 12.0,
                        font_weight: 650,
                        color: pill_fg,
                        background: pill_bg,
                        border_radius: 999.0,
                        padding: Edges::xy(8.0, 2.0),
                        flex_shrink: 0.0,
                        ..Style::default()
                    },
                ),
            ],
        ),
        Component::text(
            summary,
            Style {
                font_size: 14.0,
                font_weight: 650,
                color: ink_token(),
                ..Style::default()
            },
        ),
    ];
    if show_result {
        lines.push(Component::text(
            locale::t_args("receipt-result", &[("text", &parts.detail)]),
            Style {
                font_size: 12.0,
                color: muted_token(),
                ..Style::default()
            },
        ));
    }
    if parts.signed {
        lines.push(Component::text(
            locale::t("receipt-signed"),
            Style {
                font_size: 12.0,
                font_weight: 650,
                color: Color::from_hex("#e9fff5"),
                background: Color::from_hex("#0c6d4c"),
                border_radius: 999.0,
                padding: Edges::xy(8.0, 2.0),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ));
    }
    lines.push(Component::text(
        meta,
        Style {
            font_size: 12.0,
            color: faint_token(),
            font_family: Some("ui-monospace, monospace".to_string()),
            ..Style::default()
        },
    ));
    let focused = NOTICE_FOCUS.with(|slot| slot.borrow().clone()) == parts.id;
    Component::column(
        Style {
            gap: 4.0,
            padding: Edges::xy(10.0, 10.0),
            background: Color::rgba(244, 241, 234, 10),
            border_radius: 12.0,
            border_width: if focused { 2.0 } else { 0.0 },
            border_color: Color::from_hex("#14966a"),
            ..Style::default()
        },
        lines,
    )
}

fn open_app_ids() -> Vec<i64> {
    let mask = state::get_signal(OPEN_MASK_SIGNAL);
    (1..=6)
        .filter(|id| resident(*id) && mask & app_bit(*id) != 0)
        .collect()
}

fn build_switcher() -> Component {
    let (width, height) = state::shell_viewport();
    let open = open_app_ids();
    let mut cards = vec![Component::text(
        locale::t("switcher-title"),
        Style {
            font_size: 20.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    )];
    if open.is_empty() {
        cards.push(Component::text(
            locale::t("switcher-empty"),
            Style {
                font_size: 14.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for id in open {
            cards.push(Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    background: Color::from_hex("#1c2631"),
                    border_radius: 16.0,
                    padding: Edges::all(12.0),
                    min_height: Dimension::Px(72.0),
                    ..Style::default()
                },
                vec![
                    chrome_button(app_label(id), focus_code(id), app_accent(id)),
                    window_button(
                        "close",
                        locale::t("switcher-close"),
                        OP_CLOSE,
                        "#e07a78",
                        id,
                    ),
                ],
            ));
        }
    }
    cards.push(chrome_button(locale::t("back"), OP_SWITCHER, "#3d5570"));
    let mut panel = Component::column(
        Style {
            padding: Edges::all(16.0),
            gap: 12.0,
            background: Color::rgba(12, 18, 24, 230),
            border_radius: 22.0,
            overflow: Overflow::Auto,
            ..Style::default()
        },
        cards,
    );
    panel.style.position = Position::Absolute;
    panel.style.left = Dimension::Px(16.0);
    panel.style.top = Dimension::Px(24.0);
    panel.style.width = Dimension::Px((width - 32.0).max(200.0));
    panel.style.height = Dimension::Px((height - 140.0).max(240.0));
    panel.style.z_index = CHROME_Z + 1;
    panel
}

fn build_receipts_viewer() -> Component {
    let subject_label = truncate_middle(&current_subject(), 24);
    let entries = visible_receipts();
    let pages = receipt_page_count(entries.len());
    let page = state::get_signal(RECEIPT_PAGE_SIGNAL).clamp(0, pages - 1);
    let start = (page as usize) * RECEIPT_PAGE_SIZE;
    let end = (start + RECEIPT_PAGE_SIZE).min(entries.len());
    let page_label = (page + 1).to_string();
    let pages_label = pages.to_string();
    let filter = state::get_signal(RECEIPT_FILTER_SIGNAL);
    let mut close = action_button(locale::t("close"), OP_TOGGLE_RECEIPTS, SheetKind::Ghost);
    close.style.height = Dimension::Px(if state::pointer_is_coarse() {
        48.0
    } else {
        32.0
    });
    close.style.min_height = close.style.height;
    let mut body = vec![
        Component::row(
            Style {
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                gap: 8.0,
                ..Style::default()
            },
            vec![
                Component::text(
                    locale::t("receipts"),
                    Style {
                        font_size: 18.0,
                        font_weight: 650,
                        color: ink_token(),
                        ..Style::default()
                    },
                ),
                close,
            ],
        ),
        Component::button_with_click(
            subject_label,
            Style {
                background: Color::TRANSPARENT,
                color: muted_token(),
                font_size: 12.0,
                padding: Edges::ZERO,
                min_height: Dimension::Px(if state::pointer_is_coarse() {
                    48.0
                } else {
                    28.0
                }),
                ..Style::default()
            },
            invoke(OP_COPY_SUBJECT),
        ),
        receipt_query_field(),
        Component::row(
            Style {
                gap: 6.0,
                align_items: AlignItems::Center,
                flex_wrap: FlexWrap::Wrap,
                ..Style::default()
            },
            vec![
                filter_chip("filter-all", OP_FILTER_ALL, filter == 0),
                filter_chip("filter-allowed", OP_FILTER_ALLOWED, filter == 1),
                filter_chip("filter-denied", OP_FILTER_DENIED, filter == 2),
                filter_chip("filter-cancelled", OP_FILTER_CANCELLED, filter == 3),
            ],
        ),
    ];
    if pages > 1 {
        body.push(Component::row(
            Style {
                gap: 8.0,
                align_items: AlignItems::Center,
                ..Style::default()
            },
            vec![
                action_button(locale::t("receipt-prev"), OP_RECEIPT_PREV, SheetKind::Ghost),
                Component::text(
                    locale::t_args(
                        "receipt-page",
                        &[("page", &page_label), ("pages", &pages_label)],
                    ),
                    Style {
                        font_size: 12.0,
                        color: muted_token(),
                        ..Style::default()
                    },
                ),
                action_button(locale::t("receipt-next"), OP_RECEIPT_NEXT, SheetKind::Ghost),
            ],
        ));
    }
    if entries.is_empty() {
        let empty_key = if filter == 0 {
            "receipt-empty-long"
        } else {
            "receipt-filter-empty"
        };
        body.push(Component::text(
            locale::t(empty_key),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for line in &entries[start..end] {
            body.push(receipt_row(line));
        }
    }
    Component::column(
        Style {
            width: Dimension::Px(400.0),
            flex_shrink: 0.0,
            min_height: Dimension::Px(0.0),
            overflow: Overflow::Auto,
            padding: Edges::all(16.0),
            gap: 10.0,
            background: sheet_fill(),
            border_width: 1.0,
            border_color: line_strong(),
            border_radius: 16.0,
            box_shadow: Some(shadow_lg()),
            box_shadows: vec![inset_highlight(15)],
            ..Style::default()
        },
        body,
    )
}

#[cfg(test)]
mod shell_ux_tests {
    use super::*;

    static POSTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvFlag(&'static str);

    impl EnvFlag {
        fn set(key: &'static str, value: &str) -> Self {
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvFlag {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(self.0) };
        }
    }

    #[test]
    fn task_center_starts_collapsed() {
        ensure_shell_signals();
        assert_eq!(state::get_signal(TASKS_SIGNAL), 0);
    }

    #[test]
    fn posture_follows_width_breakpoints() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        state::note_shell_viewport(390.0, 760.0);
        assert!(matches!(posture(), Posture::Phone));
        state::note_shell_viewport(800.0, 600.0);
        assert!(matches!(posture(), Posture::Tablet));
        state::note_shell_viewport(1200.0, 800.0);
        assert!(matches!(posture(), Posture::Desktop));
    }

    #[test]
    fn receipt_pages_cover_every_entry() {
        assert_eq!(receipt_page_count(0), 1);
        assert_eq!(receipt_page_count(6), 1);
        assert_eq!(receipt_page_count(7), 2);
        assert_eq!(receipt_page_count(48), 8);
    }

    #[test]
    fn parse_receipt_keeps_id_tier_and_outcome() {
        let parts = parse_receipt("#3 allowed open-files tier=L0 actor=me confirmed=true — opened");
        assert_eq!(parts.id, "#3");
        assert_eq!(parts.decision, "allowed");
        assert_eq!(parts.command, "open-files");
        assert_eq!(parts.tier, "L0");
        assert_eq!(parts.detail, "opened");
        assert_eq!(parts.actor, "me");
        assert!(parts.recorded_at.is_empty());
        assert!(parts.utterance.is_empty());
        let stamped = parse_receipt(
            "#4 allowed open-files tier=L0 actor=local confirmed=true ts=1760518800000 — 已切换到文件 @@ 打开文件",
        );
        assert_eq!(stamped.recorded_at, "1760518800000");
        assert_eq!(stamped.detail, "已切换到文件");
        assert_eq!(stamped.utterance, "打开文件");
        assert!(!stamped.signed);
        assert_eq!(format_receipt_clock("0"), "");
        let signed = parse_receipt(
            "s#4 allowed wallet_sign tier=L2 actor=did:key:z6Mkexample confirmed=true ts=9 signed=1 — 已在本机签名 @@ hello",
        );
        assert!(signed.signed);
        assert_eq!(signed.id, "s#4");
        assert_eq!(signed.decision, "allowed");
        assert_eq!(signed.actor, "did:key:z6Mkexample");
        assert_eq!(signed.tier, "L2");
        assert_eq!(signed.utterance, "hello");
    }

    #[test]
    fn present_receipt_uses_the_current_decision_word() {
        locale::set_locale("en");
        let line = "#3 allowed open-files tier=L0 actor=me confirmed=true — opened";
        let shown = present_receipt(line);
        assert!(shown.contains("Allowed"), "{shown}");
        assert!(shown.contains("opened"), "{shown}");
        assert!(!shown.contains(" allowed"), "{shown}");
    }

    #[test]
    fn coarse_pointer_at_desktop_width_is_a_wide_tablet() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        let _env = EnvFlag::set("W3COS_POINTER", "coarse");
        state::note_shell_viewport(1280.0, 800.0);
        assert!(matches!(posture(), Posture::WideTablet));
    }

    #[test]
    fn escape_closes_the_launcher_and_leaves_the_lock_alone() {
        ensure_shell_signals();
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(LAUNCHER_SIGNAL, 1);
        assert!(on_shell_key("Escape"));
        assert_eq!(state::get_signal(LAUNCHER_SIGNAL), 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(LAUNCHER_SIGNAL, 1);
        assert!(!on_shell_key("Escape"));
        assert_eq!(state::get_signal(LAUNCHER_SIGNAL), 1);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(LAUNCHER_SIGNAL, 0);
    }

    #[test]
    fn tool_confirm_card_is_the_confirm_dialog_and_does_not_run() {
        ensure_shell_signals();
        w3cos_ai_bridge::mcp::reset_gate();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        show_tool_confirm("a11y_index");
        let dialog = build_confirm_dialog();
        assert_eq!(dialog_role(&dialog), Some("confirm"));
        let blob = text_blob(&dialog);
        assert!(blob.contains("提议 a11y_index"), "{blob}");
        assert!(blob.contains("确认后才会执行"), "{blob}");
        assert!(blob.contains("现在不会读取"), "{blob}");
        assert!(w3cos_ai_bridge::mcp::take_armed().is_none());
        assert!(!w3cos_ai_bridge::mcp::arm());
        locale::set_locale("en");
        show_tool_confirm("a11y_index");
        let blob = text_blob(&build_confirm_dialog());
        assert!(blob.contains("Propose a11y_index"), "{blob}");
        assert!(blob.contains("Nothing runs until you confirm"), "{blob}");
        assert!(w3cos_ai_bridge::mcp::take_armed().is_none());
        locale::set_locale("zh-CN");
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(TOOL_SIGNAL, 0);
    }

    #[test]
    fn host_confirm_card_reads_the_did_only_after_confirm() {
        ensure_shell_signals();
        w3cos_ai_bridge::tool_host::reset_host_mail();
        w3cos_ai_bridge::mcp::reset_gate();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        show_host_confirm();
        let dialog = build_confirm_dialog();
        assert_eq!(dialog_role(&dialog), Some("confirm"));
        let blob = text_blob(&dialog);
        assert!(blob.contains("显示这台设备的 did:key"), "{blob}");
        assert!(blob.contains("确认后才会显示"), "{blob}");
        assert!(w3cos_ai_bridge::mcp::take_armed().is_none());
        assert!(!w3cos_ai_bridge::mcp::arm());
        assert_eq!(
            w3cos_ai_bridge::tool_host::host_result_json()["pending"],
            false
        );
        locale::set_locale("en");
        show_host_confirm();
        let blob = text_blob(&build_confirm_dialog());
        assert!(blob.contains("Show this device's did:key"), "{blob}");
        assert!(blob.contains("Nothing is signed"), "{blob}");
        locale::set_locale("zh-CN");
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(HOST_SIGNAL, 0);
    }

    #[test]
    fn edge_back_returns_home_and_spares_a_permission_sheet() {
        ensure_shell_signals();
        state::note_shell_viewport(390.0, 760.0);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        focus_app(1);
        assert!(on_shell_swipe(8.0, 300.0, 90.0, 2.0));
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        state::set_signal(PERM_SIGNAL, 1);
        assert!(on_shell_swipe(8.0, 300.0, 90.0, 2.0));
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(SIGN_SIGNAL, 1);
        assert!(!edge_back());
        assert_eq!(state::get_signal(SIGN_SIGNAL), 1);
        assert!(on_shell_swipe(8.0, 300.0, 90.0, 2.0));
        assert_eq!(state::get_signal(SIGN_SIGNAL), 1);
        state::set_signal(SIGN_SIGNAL, 0);
    }

    #[test]
    fn sign_sheet_confirms_once_and_stores_a_signed_receipt() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        sign_host::reset();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-sign-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(SIGN_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);

        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);
        on_shell_intent(OP_SIGN_PROPOSE, "hello from this device");
        assert_eq!(state::get_signal(SIGN_SIGNAL), 1);
        let pending = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(
            !pending.contains("signed=1"),
            "propose must not write a receipt: {pending}"
        );
        let sheet = build_sign_sheet();
        let blob = text_blob(&sheet);
        assert!(blob.contains("用本机钥匙签名"), "{blob}");
        assert!(blob.contains("hello from this device"), "{blob}");
        assert!(blob.contains("不签也会写收据"), "{blob}");
        assert_eq!(
            button_labels(&sheet)
                .into_iter()
                .filter(|label| label == "不签" || label == "签这一次")
                .collect::<Vec<_>>(),
            vec!["不签".to_string(), "签这一次".to_string()]
        );
        assert!(
            !button_labels(&sheet).iter().any(|label| label == "确认"),
            "{blob}"
        );
        let role = sheet
            .style
            .custom_properties
            .as_ref()
            .and_then(|props| props.get("--w3cos-dialog"))
            .map(String::as_str);
        assert_eq!(role, Some("sign"));
        let confirm = build_confirm_dialog();
        assert_ne!(confirm.style.background, sheet.style.background);

        on_shell_intent(OP_SIGN_DENY, "");
        assert_eq!(state::get_signal(SIGN_SIGNAL), 0);
        let denied = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(denied.contains("s#"), "{denied}");
        assert!(denied.contains("cancelled"), "{denied}");
        assert!(denied.contains("wallet_sign"), "{denied}");
        assert!(denied.contains("signed=1"), "{denied}");
        assert!(denied.contains("did:key:z"), "{denied}");
        assert!(!denied.contains("0x"), "{denied}");

        on_shell_intent(OP_SIGN_PROPOSE, "hello from this device");
        on_shell_intent(OP_SIGN_ONCE, "");
        assert_eq!(state::get_signal(SIGN_SIGNAL), 0);
        let allowed = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(allowed.contains("allowed"), "{allowed}");
        assert!(allowed.contains("signed=1"), "{allowed}");
        assert!(!allowed.contains("0x"), "{allowed}");
        with_session(|session| {
            assert!(session.receipts().is_empty());
            assert_eq!(session.signed_receipts().len(), 2);
            let stored = &session.signed_receipts()[1];
            assert_eq!(stored.decision, Decision::Allowed);
            assert!(stored.outcome.contains("signature"), "{}", stored.outcome);
            assert!(stored.actor.starts_with("did:key:z"), "{}", stored.actor);
            assert!(!stored.signature.is_empty());
        });

        SESSION_BOOTED.with(|flag| flag.set(false));
        SESSION.with(|slot| *slot.borrow_mut() = None);
        with_session(|session| {
            assert_eq!(session.signed_receipts().len(), 2);
            assert!(session.receipts().is_empty());
            let line = session.ledger_log_tail(48);
            assert!(line.contains("cancelled"), "{line}");
            assert!(line.contains("allowed"), "{line}");
        });

        on_shell_intent(OP_LOCK, "");
        state::set_signal(SIGN_SIGNAL, 1);
        state::set_text_signal(SIGN_SUMMARY_SIGNAL, "用本机钥匙为「hello」签名");
        state::set_text_signal(SIGN_MESSAGE_SIGNAL, "hello from this device");
        let locked = build_sign_sheet();
        let locked_blob = text_blob(&locked);
        assert!(
            locked_blob.contains("先解锁。这次不会签名。"),
            "{locked_blob}"
        );
        assert!(clicks_disabled(&locked, "不签"));
        assert!(clicks_disabled(&locked, "签这一次"));
        state::set_signal(SIGN_SIGNAL, 0);
        sign_host::reset();
        drop(isolated);
    }

    #[test]
    fn copying_the_subject_keeps_the_full_did() {
        ensure_shell_signals();
        state::set_text_signal(SUBJECT_SIGNAL, "did:key:z6MkhaXgexample");
        copy_subject();
        let copied = COPIED_SUBJECT.with(|slot| slot.borrow().clone());
        assert_eq!(copied, "did:key:z6MkhaXgexample");
    }

    fn px(dim: Dimension) -> f32 {
        match dim {
            Dimension::Px(value) => value,
            other => panic!("expected px, got {other:?}"),
        }
    }

    fn walk(node: &Component, visit: &mut dyn FnMut(&Component)) {
        visit(node);
        for child in &node.children {
            walk(child, visit);
        }
    }

    struct IconFace {
        label: String,
        color_a: u8,
        background: Color,
        overflow: Overflow,
        width: f32,
        svg: String,
    }

    fn icon_buttons(root: &Component) -> Vec<IconFace> {
        let mut found = Vec::new();
        walk(root, &mut |node| {
            let ComponentKind::Button { label } = &node.kind else {
                return;
            };
            if label.is_empty() {
                return;
            }
            let Some(svg) = node.children.iter().find_map(|child| match &child.kind {
                ComponentKind::SvgDocument { source, .. } => Some(source.clone()),
                _ => None,
            }) else {
                return;
            };
            found.push(IconFace {
                label: label.clone(),
                color_a: node.style.color.a,
                background: node.style.background,
                overflow: node.style.overflow,
                width: px(node.style.width),
                svg,
            });
        });
        found
    }

    #[test]
    fn dock_icons_keep_names_without_painting_them() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(OPEN_MASK_SIGNAL, app_bit(1) | app_bit(2));
        let bar = build_floating_taskbar();
        let icons = icon_buttons(&bar);
        assert!(icons.len() >= 5, "expected launcher, apps, and tray icons");
        for icon in &icons {
            assert_eq!(icon.color_a, 0, "{} paints across the dock", icon.label);
            assert_eq!(icon.overflow, Overflow::Hidden);
            assert!(
                icon.width <= 48.0,
                "{} is {}px wide",
                icon.label,
                icon.width
            );
        }
        assert!(bar.style.gap >= 8.0);
    }

    #[test]
    fn window_controls_are_icons_not_a_missing_glyph() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        assert_ne!(locale::t("win-maximize"), "[]");
        assert_eq!(locale::t("win-minimize"), "最小化");
        assert_eq!(locale::t("win-maximize"), "最大化");
        assert_eq!(locale::t("win-restore"), "还原");
        assert_eq!(locale::t("win-close"), "关闭");
        locale::set_locale("en");
        assert_eq!(locale::t("win-maximize"), "Maximize");
        assert_ne!(locale::t("win-maximize"), "[]");
        locale::set_locale("zh-CN");
        let bar = app_titlebar("终端", "#1c8f62", 2);
        let icons = icon_buttons(&bar);
        let labels: Vec<&str> = icons.iter().map(|icon| icon.label.as_str()).collect();
        assert_eq!(labels, ["最小化", "最大化", "关闭"]);
        for icon in &icons {
            assert_eq!(icon.color_a, 0, "{}", icon.label);
            assert_eq!(icon.background.a, 0, "{} still fills a square", icon.label);
            assert_ne!(icon.background, Color::from_hex("#2a2a3e"));
            assert!(
                !icon.svg.contains('['),
                "{} still uses a tofu glyph",
                icon.label
            );
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn closing_a_window_returns_home_and_full_bleed_hides_maximize() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_signal(MINIMIZED_MASK_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        focus_app(1);

        let open = build_stage();
        assert_eq!(codes_for(&open, "关闭"), vec![OP_CLOSE]);
        assert_eq!(codes_for(&open, "最小化"), vec![OP_MINIMIZE]);
        assert_eq!(codes_for(&open, "最大化"), vec![OP_MAXIMIZE]);
        assert!(!codes_for(&open, "关闭").contains(&OP_CONFIRM));
        for icon in icon_buttons(&app_titlebar("文件", "#3498db", 1)) {
            assert_eq!(
                icon.width, 28.0,
                "{} left the 28px title target",
                icon.label
            );
        }

        on_shell_intent(OP_MAXIMIZE, "1");
        assert!(windows::is_maximized(1));
        let frame = windows::frame(1);
        let intent_top = intent_block_top(800.0);
        assert!(
            frame.y + frame.h <= intent_top - 7.5,
            "maximized window covers the intent bar: {} + {}",
            frame.y,
            frame.h
        );
        assert_eq!(codes_for(&build_stage(), "还原"), vec![OP_MAXIMIZE]);
        assert!(codes_for(&build_stage(), "最大化").is_empty());
        on_shell_intent(OP_MAXIMIZE, "1");
        assert!(!windows::is_maximized(1));

        on_shell_intent(OP_MINIMIZE, "1");
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert_ne!(state::get_signal(OPEN_MASK_SIGNAL) & app_bit(1), 0);
        assert_ne!(state::get_signal(MINIMIZED_MASK_SIGNAL) & app_bit(1), 0);
        assert!(codes_for(&build_stage(), "关闭").is_empty());
        assert!(text_blob(&build_stage()).contains("终端"));

        focus_app(1);
        on_shell_intent(OP_CLOSE, "1");
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert_eq!(state::get_signal(OPEN_MASK_SIGNAL) & app_bit(1), 0);
        assert_eq!(state::get_signal(MINIMIZED_MASK_SIGNAL) & app_bit(1), 0);
        let home = text_blob(&build_stage());
        assert!(home.contains("文件"), "{home}");
        assert!(home.contains("终端"), "{home}");
        assert!(codes_for(&build_stage(), "关闭").is_empty());
        assert!(!home.contains("stub-passkey"));
        assert!(state::get_signal_text(TOAST_SIGNAL).contains("已关闭"));

        state::note_shell_viewport(390.0, 760.0);
        focus_app(2);
        let phone = build_phone_stack();
        assert_eq!(codes_for(&phone, "关闭"), vec![OP_CLOSE]);
        assert_eq!(codes_for(&phone, "最小化"), vec![OP_MINIMIZE]);
        assert!(
            codes_for(&phone, "最大化").is_empty(),
            "phone maximize would not change the full-bleed surface"
        );
        assert!(codes_for(&phone, "还原").is_empty());
        on_shell_intent(OP_CLOSE, "2");
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert_eq!(state::get_signal(OPEN_MASK_SIGNAL) & app_bit(2), 0);
        let phone_home = build_phone_stack();
        assert!(codes_for(&phone_home, "关闭").is_empty());
        assert!(text_blob(&phone_home).contains("设置"));

        state::note_shell_viewport(800.0, 700.0);
        focus_app(1);
        let tablet = build_tablet_content();
        assert!(codes_for(&tablet, "最大化").is_empty());
        assert_eq!(codes_for(&tablet, "关闭"), vec![OP_CLOSE]);
        on_shell_intent(OP_CLOSE, "1");
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert!(text_blob(&build_tablet_content()).contains("浏览器"));

        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_signal(MINIMIZED_MASK_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_text_signal(TOAST_SIGNAL, "");
        state::note_shell_viewport(1200.0, 800.0);
        locale::set_locale("zh-CN");
    }

    #[test]
    fn resize_grip_is_not_a_white_square() {
        let corner = resize_corner(2);
        assert_eq!(corner.style.background.a, 0);
        assert_ne!(corner.style.background, Color::from_hex("#d5dee8"));
        let edge = resize_handle(2, "e", Dimension::Px(16.0), Dimension::Auto);
        assert_eq!(edge.style.background.a, 0);
        assert_ne!(edge.style.background, Color::from_hex("#2a3e52"));
    }

    #[test]
    fn receipts_drawer_stops_above_the_intent_bar() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        state::note_shell_viewport(1200.0, 800.0);
        let panel = place_receipts(Component::column(Style::default(), vec![]));
        let bottom = px(panel.style.top) + px(panel.style.height);
        let intent_top = px(floating_intent().style.top);
        assert!(
            bottom <= intent_top - 8.0,
            "drawer ends at {bottom}, intent starts at {intent_top}"
        );
        assert!(
            px(panel.style.left) > 400.0,
            "desktop drawer stays on the right"
        );
        assert!(px(panel.style.width) <= 420.0);
        state::note_shell_viewport(390.0, 760.0);
        let sheet = place_receipts(Component::column(Style::default(), vec![]));
        assert_eq!(px(sheet.style.left), 0.0);
        assert!(
            px(sheet.style.top) >= 48.0,
            "phone sheet leaves the status area"
        );
        assert_eq!(px(sheet.style.width), 390.0);
        assert_eq!(sheet.style.border_top_left_radius, Some(22.0));
        assert_eq!(sheet.style.border_bottom_left_radius, Some(0.0));
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn notify_panel_stops_above_the_intent_bar() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        state::note_shell_viewport(1200.0, 800.0);
        let panel = build_notify_panel();
        let bottom = px(panel.style.top) + px(panel.style.max_height);
        let intent_top = px(floating_intent().style.top);
        assert!(
            bottom <= intent_top - 8.0,
            "notify ends at {bottom}, intent starts at {intent_top}"
        );
    }

    #[test]
    fn phone_keyboard_is_in_the_phone_shell_only_while_a_field_is_focused() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        ime_bar::reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(390.0, 760.0);
        state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".to_string(),
            secure: false,
            x: 16.0,
            y: 640.0,
            width: 300.0,
            height: 36.0,
        });
        let phone = format!("{:?}", build_shell());
        assert!(
            phone.contains("--w3cos-softkey"),
            "phone shell is missing the keyboard"
        );
        assert!(phone.contains(&locale::t("kbd-backspace")));
        state::clear_ime_field();
        let hidden = format!("{:?}", build_shell());
        assert!(
            !hidden.contains("--w3cos-softkey"),
            "keyboard stayed up without a field"
        );
        state::note_shell_viewport(1200.0, 800.0);
        state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".to_string(),
            secure: false,
            x: 16.0,
            y: 640.0,
            width: 300.0,
            height: 36.0,
        });
        let desktop = format!("{:?}", build_shell());
        assert!(!desktop.contains("--w3cos-softkey"));
        state::clear_ime_field();
        ime_bar::sync_field_presence();
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn chrome_accents_are_not_indigo() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        state::note_shell_viewport(768.0, 1024.0);
        let rail = format!("{:?}", build_tablet_rail());
        let launcher = format!("{:?}", build_launcher());
        let settings = format!("{:?}", build_settings_app());
        for (name, dumped) in [
            ("rail", rail),
            ("launcher", launcher),
            ("settings", settings),
        ] {
            for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
                assert!(!dumped.contains(hex), "{name} still uses {hex}");
            }
        }
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn focused_window_border_is_jade() {
        let frame = build_window_frame(2, true);
        assert!(frame.style.border_color.g > frame.style.border_color.r + 40);
        assert!(frame.style.border_color.b < 180);
        assert_ne!(
            (
                frame.style.border_color.r,
                frame.style.border_color.g,
                frame.style.border_color.b
            ),
            (108, 92, 231)
        );
    }

    #[test]
    fn terminal_states_the_process_grant_once() {
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::set_text_signal(TERM_OUT_SIGNAL, "");
        state::set_text_signal(TERMINAL_SIGNAL, "");
        let app = build_terminal_app();
        let blob = text_blob(&app);
        assert!(
            blob.contains("这里直接运行的只有 echo、pwd、help。"),
            "{blob}"
        );
        assert!(blob.contains("其他程序走沙箱。"), "{blob}");
        assert!(blob.contains("命令"), "{blob}");
        assert!(!blob.contains("命令还不会执行"), "{blob}");
        assert!(!blob.contains("w3cos-process"), "{blob}");
        assert!(!blob.contains("user@w3cos"), "{blob}");
        locale::set_locale("en");
        state::set_text_signal(TERM_OUT_SIGNAL, "");
        let english = text_blob(&build_terminal_app());
        assert!(
            english.contains("Only echo, pwd, and help run directly here."),
            "{english}"
        );
        assert!(
            english.contains("Anything else opens the sandbox."),
            "{english}"
        );
        assert!(!english.contains("Shell ready"), "{english}");
        assert!(!english.contains("w3cos-process"), "{english}");
        locale::set_locale("zh-CN");
    }

    #[test]
    fn terminal_copy_matches_what_run_can_do() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-term-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_text_signal(TERMINAL_SIGNAL, "");
        state::set_text_signal(TERM_OUT_SIGNAL, "");
        capability_gate::deny(shell_tools::CAP_PROCESS);
        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);

        state::set_text_signal(TERMINAL_SIGNAL, "ls");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert_eq!(state::get_signal(SANDBOX_SIGNAL), 1);
        assert_eq!(state::get_signal(SANDBOX_FS_SIGNAL), 0);
        assert_eq!(state::get_signal(SANDBOX_NET_SIGNAL), 0);
        let sheet = text_blob(&build_sandbox_sheet());
        assert!(sheet.contains("在沙箱里运行 ls"), "{sheet}");
        assert!(sheet.contains("没有磁盘"), "{sheet}");
        assert!(sheet.contains("没有网络"), "{sheet}");
        assert!(!sheet.contains("/bin/ls"), "{sheet}");
        on_shell_intent(OP_PERM_DENY, "");
        assert_eq!(state::get_signal(SANDBOX_SIGNAL), 0);
        let refused = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(refused.contains("没有运行"), "{refused}");
        assert!(!refused.to_lowercase().contains("bin/ls"), "{refused}");
        assert!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("Denied"),
            "{}",
            state::get_signal_text(RECEIPT_SIGNAL)
        );
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));

        state::set_text_signal(TERMINAL_SIGNAL, "echo 你好");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        assert_eq!(state::get_signal(SANDBOX_SIGNAL), 0);
        assert_eq!(state::get_signal_text(TERM_OUT_SIGNAL), refused);
        let sheet = text_blob(&build_permission_sheet());
        assert!(sheet.contains("允许运行这一次 echo"), "{sheet}");
        on_shell_intent(OP_PERM_ONCE, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert_eq!(state::get_signal_text(TERM_OUT_SIGNAL), "你好");
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));
        assert!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("allowed"),
            "{}",
            state::get_signal_text(RECEIPT_SIGNAL)
        );

        state::set_text_signal(TERMINAL_SIGNAL, "");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert!(state::get_signal_text(TERM_OUT_SIGNAL).contains("先写一条命令"));

        state::set_text_signal(TERMINAL_SIGNAL, "echo hi; ls");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        let meta = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(meta.contains("特殊符号"), "{meta}");
        assert!(!meta.contains("hi"), "{meta}");

        state::set_text_signal(TERMINAL_SIGNAL, "help");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        on_shell_intent(OP_PERM_ONCE, "");
        let help = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(help.contains("可以运行 echo、pwd、help。"), "{help}");
        assert!(help.contains("没有系统 shell。"), "{help}");
        assert!(!help.contains("Other programs"), "{help}");

        locale::set_locale("en");
        assert_eq!(
            locale::t_args("terminal-refused", &[("command", "ls")]),
            "Did not run ls. Only echo, pwd, and help are accepted."
        );
        assert!(!locale::t("terminal-hint").contains("w3cos-process"));

        locale::set_locale("zh-CN");
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(SANDBOX_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_text_signal(TERMINAL_SIGNAL, "");
        state::set_text_signal(TERM_OUT_SIGNAL, "");
        drop(isolated);
    }

    #[test]
    fn sandbox_sheet_starts_closed_and_a_grant_is_one_run() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-sb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("marker.txt");
        std::fs::write(&marker, "marker-not-a-seed").unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        struct HideSeed;
        impl Drop for HideSeed {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("W3COS_DEVICE_SEED");
                }
            }
        }
        let _hide_seed = HideSeed;
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
            std::env::set_var("W3COS_DEVICE_SEED", "seed-should-stay-in-the-window");
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::note_pointer_coarse(false);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        clear_sandbox_sheet();
        state::set_text_signal(TERMINAL_SIGNAL, "printenv");
        state::set_text_signal(TERM_OUT_SIGNAL, "");
        on_shell_intent(OP_UNLOCK, "device-pass");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(SANDBOX_SIGNAL), 1);
        assert!(!edge_back());
        let sheet = text_blob(&build_sandbox_sheet());
        assert!(sheet.contains("在沙箱里运行 printenv"), "{sheet}");
        assert!(sheet.contains("默认没有磁盘，也没有网络。"), "{sheet}");
        assert!(sheet.contains("设备钥匙和钱包留在这个窗口里"), "{sheet}");
        assert!(sheet.contains("允许读文件"), "{sheet}");
        assert!(sheet.contains("允许联网"), "{sheet}");
        assert!(!sheet.contains("seed-should-stay-in-the-window"), "{sheet}");
        assert!(!sheet.contains("wallet_sign"), "{sheet}");
        state::note_shell_viewport(390.0, 760.0);
        state::note_pointer_coarse(true);
        let phone = build_sandbox_sheet();
        let (_, _, _, height) = button_face(&phone, "运行这一次");
        assert!(height >= 48.0, "{height}");
        locale::set_locale("en");
        let english = text_blob(&build_sandbox_sheet());
        assert!(english.contains("Run printenv in the sandbox"), "{english}");
        assert!(
            english.contains("Files and the network stay closed."),
            "{english}"
        );
        assert!(
            english.contains("The device key and the wallet stay in this window"),
            "{english}"
        );
        locale::set_locale("zh-CN");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);

        on_shell_intent(OP_PERM_DENY, "");
        assert_eq!(state::get_signal(SANDBOX_SIGNAL), 0);
        let denied = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(denied.contains("没有运行"), "{denied}");
        assert!(
            !denied.contains("seed-should-stay-in-the-window"),
            "{denied}"
        );
        assert!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("Denied"),
            "{}",
            state::get_signal_text(RECEIPT_SIGNAL)
        );

        state::set_text_signal(TERMINAL_SIGNAL, "printenv");
        on_shell_intent(OP_RUN_TERMINAL, "");
        on_shell_intent(OP_PERM_ONCE, "");
        assert_eq!(state::get_signal(SANDBOX_SIGNAL), 0);
        assert_eq!(state::get_signal(SANDBOX_FS_SIGNAL), 0);
        assert_eq!(state::get_signal(SANDBOX_NET_SIGNAL), 0);
        let hidden = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(
            !hidden.contains("seed-should-stay-in-the-window"),
            "{hidden}"
        );
        assert!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("Allowed"),
            "{}",
            state::get_signal_text(RECEIPT_SIGNAL)
        );

        let cat = format!("cat {}", marker.display());
        state::set_text_signal(TERMINAL_SIGNAL, &cat);
        on_shell_intent(OP_RUN_TERMINAL, "");
        on_shell_intent(OP_PERM_ONCE, "");
        let closed = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(
            closed.contains("没有这块磁盘") || closed.contains("没有读到文件"),
            "{closed}"
        );
        assert!(!closed.contains("marker-not-a-seed"), "{closed}");

        on_shell_intent(OP_RUN_TERMINAL, "");
        on_shell_intent(OP_SANDBOX_FS, "");
        assert_eq!(state::get_signal(SANDBOX_FS_SIGNAL), 1);
        assert!(text_blob(&build_sandbox_sheet()).contains("这一次可以读文件。仍然没有网络。"));
        on_shell_intent(OP_PERM_ONCE, "");
        let opened = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(opened.contains("marker-not-a-seed"), "{opened}");
        assert_eq!(state::get_signal(SANDBOX_FS_SIGNAL), 0);

        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(SANDBOX_FS_SIGNAL), 0);
        on_shell_intent(OP_PERM_ONCE, "");
        let again = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(!again.contains("marker-not-a-seed"), "{again}");

        unsafe {
            std::env::remove_var("W3COS_DEVICE_SEED");
        }
        locale::set_locale("zh-CN");
        clear_sandbox_sheet();
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    fn ime_role(root: &Component, role: &str) -> bool {
        let mut found = false;
        walk(root, &mut |node| {
            let marked =
                node.style.custom_properties.as_ref().is_some_and(|props| {
                    props.get("--w3cos-ime").is_some_and(|value| value == role)
                });
            if marked {
                found = true;
            }
        });
        found
    }

    #[test]
    fn text_fields_declare_an_ime_role() {
        ensure_shell_signals();
        assert!(ime_role(&build_intent_panel(), "intent"));
        assert!(ime_role(&build_terminal_app(), "terminal"));
        assert!(ime_role(&build_launcher(), "launcher"));
        assert!(ime_role(&build_lock_screen(), "secure"));
        assert!(ime_role(&receipt_query_field(), "text"));
        let lock = build_lock_screen();
        let mut secure = false;
        walk(&lock, &mut |node| {
            if let ComponentKind::TextInput { secure: hidden, .. } = &node.kind
                && *hidden
            {
                secure = true;
            }
        });
        assert!(secure);
    }

    fn absolute_boxes_fit(
        node: &Component,
        cb_x: f32,
        cb_y: f32,
        viewport_w: f32,
        viewport_h: f32,
    ) {
        let left = match node.style.left {
            Dimension::Px(value) => value,
            _ => 0.0,
        };
        let top = match node.style.top {
            Dimension::Px(value) => value,
            _ => 0.0,
        };
        let width = match node.style.width {
            Dimension::Px(value) => Some(value),
            _ => None,
        };
        let height = match node.style.height {
            Dimension::Px(value) if value > 0.0 => Some(value),
            _ => None,
        };
        let (x, y) = if node.style.position == Position::Absolute {
            (cb_x + left, cb_y + top)
        } else {
            (cb_x, cb_y)
        };
        if node.style.position == Position::Absolute {
            if let Some(w) = width {
                assert!(
                    x >= -0.5 && x + w <= viewport_w + 0.5,
                    "x={x} w={w} viewport={viewport_w} kind={:?}",
                    node.kind
                );
            }
            if let Some(h) = height {
                assert!(
                    y >= -0.5 && y + h <= viewport_h + 0.5,
                    "y={y} h={h} viewport={viewport_h} kind={:?}",
                    node.kind
                );
            }
        }
        let positioned = matches!(node.style.position, Position::Absolute | Position::Relative);
        let (next_x, next_y) = if positioned { (x, y) } else { (cb_x, cb_y) };
        for child in &node.children {
            absolute_boxes_fit(child, next_x, next_y, viewport_w, viewport_h);
        }
    }

    fn candidate_bar(node: &Component) -> Option<&Component> {
        if node.style.position == Position::Absolute && node.style.z_index == CHROME_Z + 5 {
            return Some(node);
        }
        for child in &node.children {
            if let Some(found) = candidate_bar(child) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn chrome_stays_inside_the_viewport_and_candidates_clear_the_intent() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(LAUNCHER_SIGNAL, 0);
        state::set_signal(NOTIFY_SIGNAL, 0);
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        // The first build restores saved windows. Later builds keep the mask.
        let _ = build_shell();
        for (width, height) in [(1200.0, 800.0), (768.0, 1024.0), (390.0, 760.0)] {
            state::set_signal(OPEN_MASK_SIGNAL, 0);
            state::set_signal(MINIMIZED_MASK_SIGNAL, 0);
            state::set_signal(APP_SIGNAL, 0);
            state::note_shell_viewport(width, height);
            ime_bar::reset_for_tests();
            state::note_ime_field(state::ImeFieldNote {
                index: 4,
                value: String::new(),
                role: "intent".into(),
                secure: false,
                x: 24.0,
                y: intent_block_top(height) + 28.0,
                width: 280.0,
                height: 36.0,
            });
            for ch in "nihao".chars() {
                assert!(ime_bar::on_key(&ch.to_string()));
            }
            apply_ime_field();
            let root = build_shell();
            absolute_boxes_fit(&root, 0.0, 0.0, width, height);
            if width >= 1024.0 || width < 600.0 {
                let shown = live_clock();
                let mut digit_widths = Vec::new();
                let mut colon_width = None;
                walk(&root, &mut |node| {
                    if node.children.len() != shown.chars().count() {
                        return;
                    }
                    let matches_clock =
                        node.children.iter().zip(shown.chars()).all(|(child, ch)| {
                            matches!(&child.kind, ComponentKind::Text { content } if content.chars().next() == Some(ch))
                                && !child.style.font_kerning
                        });
                    if !matches_clock {
                        return;
                    }
                    digit_widths = node
                        .children
                        .iter()
                        .filter(|child| {
                            matches!(&child.kind, ComponentKind::Text { content } if content != ":")
                        })
                        .map(|child| px(child.style.width))
                        .collect();
                    colon_width = node.children.iter().find_map(|child| match &child.kind {
                        ComponentKind::Text { content } if content == ":" => {
                            Some(px(child.style.width))
                        }
                        _ => None,
                    });
                });
                assert!(digit_widths.len() >= 4, "{digit_widths:?}");
                assert!(
                    digit_widths
                        .windows(2)
                        .all(|pair| (pair[0] - pair[1]).abs() < 0.1)
                );
                let colon_width = colon_width.expect("colon");
                assert!(colon_width < digit_widths[0]);
            }
            let preedit = {
                let mut found = None;
                walk(&root, &mut |node| {
                    if let ComponentKind::TextInput { value, secure, .. } = &node.kind
                        && !*secure
                        && let Some(props) = &node.style.custom_properties
                        && let Some(suffix) = props.get("--w3cos-ime-preedit")
                    {
                        found = Some((value.clone(), suffix.clone()));
                    }
                });
                found
            };
            let (value, suffix) = preedit.expect("underlined preedit");
            assert_eq!(suffix, "ni hao");
            assert!(value.ends_with("ni hao"), "{value}");
            assert!(!value.ends_with("nihao"));
            if width >= 600.0 {
                let bar = candidate_bar(&root).expect("candidate bar");
                let bar_top = px(bar.style.top);
                let bar_h = px(bar.style.height);
                let bar_left = px(bar.style.left);
                let bar_w = px(bar.style.width);
                let intent_top = intent_block_top(height);
                assert!(
                    bar_top + bar_h <= intent_top - 8.0 + 0.5,
                    "bar {bar_top}+{bar_h} intersects intent {intent_top}"
                );
                assert!(bar_left >= -0.5 && bar_left + bar_w <= width + 0.5);
                assert_eq!(bar.style.flex_wrap, FlexWrap::NoWrap);
            } else {
                assert!(candidate_bar(&root).is_none());
            }
        }
        state::note_shell_viewport(1200.0, 800.0);
        ime_bar::reset_for_tests();
        state::clear_ime_field();
        locale::set_locale("en");
    }

    #[test]
    fn command_action_opens_confirm_without_executing() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        ime_bar::reset_for_tests();
        locale::set_locale("zh-CN");
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".into(),
            secure: false,
            x: 120.0,
            y: 700.0,
            width: 280.0,
            height: 36.0,
        });
        for ch in "wenjian".chars() {
            assert!(ime_bar::on_key(&ch.to_string()));
        }
        assert_eq!(ime_bar::command_utterance().as_deref(), Some("open files"));
        let app_before = state::get_signal(APP_SIGNAL);
        on_shell_intent(OP_IME_ACTION, "");
        assert_eq!(
            state::get_signal(PHASE_SIGNAL),
            1,
            "the action opens the confirm card"
        );
        assert_eq!(state::get_signal(APP_SIGNAL), app_before);
        assert!(!state::get_signal_text(SUMMARY_SIGNAL).is_empty());
        on_shell_intent(OP_CANCEL, "");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 0);
        assert_eq!(state::get_signal(APP_SIGNAL), app_before);
        ime_bar::reset_for_tests();
        state::clear_ime_field();
        locale::set_locale("en");
    }

    #[test]
    fn smart_bar_explains_blocked_capture_and_stays_off_the_network() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(NOTIFY_SIGNAL, 0);
        state::note_shell_viewport(390.0, 760.0);
        ime_bar::reset_for_tests();
        state::note_ime_field(state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".into(),
            secure: false,
            x: 24.0,
            y: 480.0,
            width: 280.0,
            height: 36.0,
        });
        let root = build_shell();
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        let labels = button_labels(&root);
        for key in ["ime-voice", "ime-ink", "ime-clip", "ime-screen"] {
            let label = locale::t(key);
            assert!(labels.iter().any(|item| item == &label), "{labels:?}");
        }
        on_shell_intent(OP_IME_VOICE, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-voice-offer")
        );
        let root_dump = format!("{root:?}");
        assert_eq!(root_dump.matches("--w3cos-hold").count(), 1, "{root_dump}");
        state::set_text_signal(DRAFT_SIGNAL, "");
        on_shell_hold(OP_IME_VOICE, 0);
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-voice")
        );
        assert!(!ime_bar::voice_recording());
        on_shell_hold(OP_IME_VOICE, 2);
        assert!(!ime_bar::voice_recording());
        assert_eq!(state::get_signal_text(DRAFT_SIGNAL), "");
        let ink_dir = std::env::temp_dir().join(format!("w3cos-ink-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ink_dir);
        std::fs::create_dir_all(&ink_dir).unwrap();
        ime_bar::set_handwriting_dir_for_tests(Some(ink_dir.clone()));
        on_shell_intent(OP_IME_INK, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-ink-offer")
        );
        let _ = std::fs::remove_dir_all(&ink_dir);
        on_shell_intent(OP_IME_SCREEN, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-screen-offer")
        );
        assert_eq!(state::get_signal(PHASE_SIGNAL), 0);
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert!(!handoff::clipboard_sync_enabled());

        state::note_shell_viewport(1200.0, 800.0);
        ime_bar::reset_for_tests();
        state::clear_ime_field();
        state::note_ime_field(state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".into(),
            secure: false,
            x: 80.0,
            y: 180.0,
            width: 280.0,
            height: 36.0,
        });
        let root = build_shell();
        let labels = button_labels(&root);
        assert!(labels.iter().any(|item| item == &locale::t("ime-clip")));
        assert!(candidate_bar(&root).is_none());

        state::note_shell_viewport(1200.0, 800.0);
        state::clear_ime_field();
        ime_bar::reset_for_tests();
        state::set_text_signal(TOAST_SIGNAL, "");
        locale::set_locale("en");
    }

    #[test]
    fn screen_permission_is_a_receipt_and_does_not_capture_by_itself() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use w3cos_ime::{
            LocalScreenText, ScreenImage, ScreenRegion, ScreenSnippet, ScreenTextManifest,
        };

        struct Probe {
            calls: std::sync::Arc<AtomicU32>,
        }

        impl LocalScreenText for Probe {
            fn manifest(&self) -> ScreenTextManifest {
                ScreenTextManifest {
                    id: "test.screen".into(),
                    locales: vec!["zh-Hans".into()],
                    network: false,
                    available: true,
                }
            }

            fn recognize(
                &mut self,
                _image: &ScreenImage,
                _region: ScreenRegion,
            ) -> Vec<ScreenSnippet> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                vec![ScreenSnippet {
                    text: "不应出现".into(),
                    score: 1.0,
                }]
            }
        }

        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(PHASE_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        ime_bar::reset_for_tests();
        let calls = std::sync::Arc::new(AtomicU32::new(0));
        ime_bar::install_screen_for_tests(Box::new(Probe {
            calls: calls.clone(),
        }));
        state::note_ime_field(state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".into(),
            secure: false,
            x: 80.0,
            y: 180.0,
            width: 280.0,
            height: 36.0,
        });
        on_shell_intent(OP_IME_SCREEN, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-screen-ask")
        );
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(blob.contains(&locale::t("ime-screen-title")), "{blob}");
        assert!(blob.contains(&locale::t("ime-screen-impact")), "{blob}");
        on_shell_intent(OP_IME_SCREEN_NO, "");
        let log = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(log.contains("cancelled"), "{log}");
        assert!(log.contains("screen-region"), "{log}");
        assert!(!log.contains("不应出现"), "{log}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(state::get_signal(PHASE_SIGNAL), 0);

        on_shell_intent(OP_IME_SCREEN, "");
        on_shell_intent(OP_IME_SCREEN_YES, "");
        let log = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(log.contains("allowed"), "{log}");
        assert!(log.contains("screen-region"), "{log}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(blob.contains(&locale::t("ime-screen-hint")), "{blob}");

        let dir =
            std::env::temp_dir().join(format!("w3cos-screen-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ime_bar::reset_for_tests();
        ime_bar::set_screen_dir_for_tests(Some(dir.clone()));
        ime_bar::set_handwriting_dir_for_tests(Some(dir.clone()));
        present_settings(6, 0);
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(blob.contains(&locale::t("ime-screen-model")), "{blob}");
        assert!(blob.contains(&locale::t("ime-screen-model-off")), "{blob}");
        on_shell_intent(OP_IME_SCREEN_MODEL, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-screen-confirm")
        );
        assert!(!dir.join(w3cos_ime::DET_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
        ime_bar::reset_for_tests();
        state::clear_ime_field();
        locale::set_locale("en");
    }

    #[test]
    fn speech_model_stays_off_until_a_second_confirmation() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        let dir = std::env::temp_dir().join(format!("w3cos-speech-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ime_bar::set_speech_dir_for_tests(Some(dir.clone()));
        ime_bar::reset_for_tests();
        ime_bar::set_speech_dir_for_tests(Some(dir.clone()));
        present_settings(6, 0);
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(blob.contains(&locale::t("ime-voice-model")), "{blob}");
        assert!(blob.contains(&locale::t("ime-voice-model-off")), "{blob}");
        assert!(blob.contains(&locale::t("ime-voice-model-get")), "{blob}");
        on_shell_intent(OP_IME_MODEL, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-voice-confirm")
        );
        assert!(!dir.join(w3cos_ime::MODEL_FILE).exists());
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(
            blob.contains(&locale::t("ime-voice-model-confirm")),
            "{blob}"
        );
        assert!(!blob.contains(&locale::t("ime-voice-model-get")), "{blob}");
        ime_bar::reset_for_tests();
        let _ = std::fs::remove_dir_all(&dir);
        locale::set_locale("en");
    }

    #[test]
    fn handwriting_model_stays_off_until_a_second_confirmation() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        let dir = std::env::temp_dir().join(format!("w3cos-ink-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ime_bar::reset_for_tests();
        ime_bar::set_handwriting_dir_for_tests(Some(dir.clone()));
        present_settings(6, 0);
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(blob.contains(&locale::t("ime-ink-model")), "{blob}");
        assert!(blob.contains(&locale::t("ime-ink-model-off")), "{blob}");
        assert!(blob.contains(&locale::t("ime-ink-model-get")), "{blob}");
        on_shell_intent(OP_IME_INK_MODEL, "");
        assert_eq!(
            state::get_signal_text(TOAST_SIGNAL),
            locale::t("ime-toast-ink-confirm")
        );
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        let root = build_shell();
        let blob = text_blob(&root);
        assert!(blob.contains(&locale::t("ime-ink-model-confirm")), "{blob}");
        assert!(!blob.contains(&locale::t("ime-ink-model-get")), "{blob}");
        ime_bar::reset_for_tests();
        let _ = std::fs::remove_dir_all(&dir);
        locale::set_locale("en");
    }

    fn button_labels(root: &Component) -> Vec<String> {
        let mut labels = Vec::new();
        walk(root, &mut |node| {
            if let ComponentKind::Button { label } = &node.kind
                && !label.is_empty()
            {
                labels.push(label.clone());
            }
        });
        labels
    }

    fn text_blob(root: &Component) -> String {
        let mut parts = Vec::new();
        walk(root, &mut |node| {
            if let ComponentKind::Text { content } = &node.kind {
                parts.push(content.clone());
            }
            if let ComponentKind::Button { label } = &node.kind {
                parts.push(label.clone());
            }
        });
        parts.join("\n")
    }

    #[test]
    fn strangers_show_a_grouped_code_and_are_not_connected() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        handoff::reset_for_tests();
        locale::set_locale("zh-CN");
        handoff::demo_pending();
        let page = Component::column(Style::default(), settings_devices_page());
        let blob = text_blob(&page);
        assert!(blob.contains("待核对"), "{blob}");
        assert!(!blob.contains("已连接"), "{blob}");
        assert!(blob.contains("数字不同"));
        assert!(blob.contains("数字一致，信任这台"));
        assert!(
            blob.contains(' ') && blob.chars().filter(|ch| ch.is_ascii_digit()).count() >= 6,
            "{blob}"
        );
        let peer = handoff::pending_peer().expect("pending");
        let (grouped, _) = handoff::short_code(
            &handoff::local_fingerprint(),
            &peer.fingerprint,
            &peer.nonce,
        );
        assert!(blob.contains(&grouped), "{grouped} missing from {blob}");
        locale::set_locale("en");
    }

    #[test]
    fn sync_sheet_asks_one_surface_and_does_not_enable_the_rest() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        handoff::reset_for_tests();
        locale::set_locale("zh-CN");
        handoff::demo_pending();
        assert!(handoff::codes_match());
        let sheet = build_sync_sheet();
        let blob = text_blob(&sheet);
        assert!(blob.contains("同步收据？"), "{blob}");
        assert!(blob.contains("不允许"));
        assert!(blob.contains("允许这一面"));
        assert!(!blob.contains("同步场景？"));
        assert!(!handoff::sync_receipts_enabled());
        assert!(!handoff::sync_scene_enabled());
        assert!(!handoff::clipboard_sync_enabled());
        on_shell_intent(OP_SYNC_SKIP, "");
        let next = text_blob(&build_sync_sheet());
        assert!(next.contains("同步未执行的草稿？"), "{next}");
        locale::set_locale("en");
    }

    #[test]
    fn continue_banner_sits_under_the_title_bar_and_is_not_a_scrim() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        handoff::reset_for_tests();
        locale::set_locale("zh-CN");
        ensure_shell_signals();
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(APP_SIGNAL, 1);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        handoff::demo_banner();
        let banner = build_handoff_banner();
        let frame = windows::frame(1);
        let title_h = if state::pointer_is_coarse() {
            56.0
        } else {
            40.0
        };
        assert_eq!(px(banner.style.top), frame.y + 6.0 + title_h);
        assert!(px(banner.style.width) < 1200.0 - 40.0);
        assert_eq!(banner.kind, ComponentKind::Row);
        let labels = button_labels(&banner);
        assert!(labels.iter().any(|label| label == "不是现在"));
        assert!(labels.iter().any(|label| label == "继续"));
        let blob = text_blob(&banner);
        assert!(blob.contains("在手机上继续"), "{blob}");
        assert!(!blob.contains("已连接"));
        let shell = format!("{:?}", build_shell());
        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(!shell.contains(hex), "{hex}");
        }
        locale::set_locale("en");
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn phone_receive_page_opens_the_file_without_confirming() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        let _name = EnvFlag::set("W3COS_DEVICE_NAME", "手机");
        handoff::reset_for_tests();
        locale::set_locale("zh-CN");
        ensure_shell_signals();
        state::note_shell_viewport(390.0, 760.0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_text_signal(DRAFT_SIGNAL, "把这段改成夜间模式");
        handoff::demo_incoming();
        let hidden = build_handoff_incoming();
        assert_eq!(px(hidden.style.height), 760.0);
        let labels = button_labels(&hidden);
        let open_at = labels
            .iter()
            .position(|label| label == "打开文件")
            .expect("open");
        let clip_at = labels
            .iter()
            .position(|label| label == "用这份")
            .expect("clip");
        assert!(open_at < clip_at, "{labels:?}");
        let blob = text_blob(&hidden);
        assert!(blob.contains("从书桌继续"), "{blob}");
        assert!(blob.contains("/docs/notes.txt"));
        assert!(blob.contains("把这段改成夜间模式"));
        assert!(blob.contains("L2 在这台手机上仍然关闭"));
        assert!(blob.contains("书桌上的确认不会自动生效"));
        assert!(blob.contains("剪贴板有两份"));
        on_shell_intent(OP_HANDOFF_OPEN, "");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 0);
        assert_eq!(state::get_signal_text(DRAFT_SIGNAL), "把这段改成夜间模式");
        assert_eq!(state::get_signal(APP_SIGNAL), 1);
        assert!(handoff::incoming().is_none());
        assert!(handoff::clip_conflict().is_some());
        locale::set_locale("en");
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    fn an_unrevealed_phone_handoff_is_a_notification() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        handoff::reset_for_tests();
        locale::set_locale("zh-CN");
        ensure_shell_signals();
        state::note_shell_viewport(390.0, 760.0);
        let peer = handoff::DeviceKey::from_seed([8; 32]);
        let payload = handoff::SyncPayload {
            receipts: Vec::new(),
            draft: "把这段改成夜间模式".to_string(),
            path: "/docs/notes.txt".to_string(),
            clipboard: None,
        };
        let sealed = handoff::seal(&peer, &handoff::local_public(), &payload).unwrap();
        assert!(handoff::receive_sealed(
            &peer.public_bytes(),
            "书桌",
            &sealed,
            None,
        ));
        let notice = build_handoff_incoming();
        let blob = text_blob(&notice);
        assert!(blob.contains("书桌想把文件交过来。"), "{blob}");
        assert!(blob.contains("查看"));
        assert!(!blob.contains("打开文件"), "{blob}");
        on_shell_intent(OP_HANDOFF_REVEAL, "");
        let page = text_blob(&build_handoff_incoming());
        assert!(page.contains("打开文件"), "{page}");
        assert!(page.contains("从书桌继续"));
        locale::set_locale("en");
        state::note_shell_viewport(1200.0, 800.0);
    }

    fn type_spelling(spelling: &str) {
        for ch in spelling.chars() {
            assert!(ime_bar::on_key(&ch.to_string()));
        }
    }

    #[test]
    fn personal_lexicon_learns_on_accept_and_skips_secure_fields() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        lexicon::reset_for_tests();
        ime_bar::reset_for_tests();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(APP_SIGNAL, 0);
        state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".into(),
            secure: false,
            x: 120.0,
            y: 700.0,
            width: 280.0,
            height: 36.0,
        });
        type_spelling("nihao");
        assert_eq!(
            ime_bar::candidate_texts().first().map(String::as_str),
            Some("你好"),
            "an empty store keeps the dictionary head"
        );
        for step in 0..3 {
            let texts = ime_bar::candidate_texts();
            if step == 1 {
                assert_eq!(
                    texts.first().map(String::as_str),
                    Some("你好"),
                    "one accept stays under the dictionary head"
                );
            }
            let at = texts.iter().position(|text| text == "您好").expect("您好");
            assert!(ime_bar::commit_candidate(at));
            if step < 2 {
                type_spelling("nihao");
            }
        }
        assert_eq!(lexicon::freq_of("您好"), Some(3));
        assert!(lexicon::neg_of("你好").unwrap_or(0) >= 1);
        type_spelling("nihao");
        assert_eq!(
            ime_bar::candidate_texts().first().map(String::as_str),
            Some("您好")
        );

        state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
            index: 8,
            value: String::new(),
            role: "password".into(),
            secure: true,
            x: 120.0,
            y: 400.0,
            width: 280.0,
            height: 36.0,
        });
        for ch in "mima".chars() {
            assert!(!ime_bar::on_key(&ch.to_string()));
        }
        lexicon::note_commit("app-desktop", true, "mima", "密码词", &["另一词".into()]);
        assert!(lexicon::freq_of("密码词").is_none());
        assert_eq!(lexicon::freq_of("您好"), Some(3));

        lexicon::reset_for_tests();
        lexicon::seed_demo();
        state::set_signal(SETTINGS_TAB_SIGNAL, 5);
        let page = Component::column(Style::default(), settings_lexicon_page());
        let blob = text_blob(&page);
        assert!(blob.contains("您好"), "{blob}");
        assert!(blob.contains("nihao"), "{blob}");
        assert!(blob.contains("老王"), "{blob}");
        assert!(blob.contains("王明"), "{blob}");
        assert!(blob.contains("导出"), "{blob}");
        assert!(blob.contains("清空词库"), "{blob}");
        assert!(blob.contains("隐身输入"), "{blob}");
        assert!(!blob.contains("已连接"), "{blob}");
        let dumped = format!("{page:?}");
        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(!dumped.contains(hex), "{hex}");
        }
        on_shell_intent(OP_LEXICON_SAVE, "词库乙 ceshi");
        assert_eq!(lexicon::freq_of("词库乙"), Some(1));
        on_shell_intent(OP_LEXICON_STEALTH, "");
        assert!(lexicon::stealth_on());
        ime_bar::reset_for_tests();
        state::note_ime_field(w3cos_runtime::state::ImeFieldNote {
            index: 4,
            value: String::new(),
            role: "intent".into(),
            secure: false,
            x: 120.0,
            y: 700.0,
            width: 280.0,
            height: 36.0,
        });
        type_spelling("nihao");
        let at = ime_bar::candidate_texts()
            .iter()
            .position(|text| text == "您好")
            .expect("您好");
        assert!(ime_bar::commit_candidate(at));
        assert_eq!(lexicon::freq_of("您好"), Some(3));
        on_shell_intent(OP_LEXICON_CLEAR, "");
        assert!(lexicon::entries().is_empty());
        assert!(lexicon::stealth_on());

        lexicon::reset_for_tests();
        ime_bar::reset_for_tests();
        state::clear_ime_field();
        locale::set_locale("en");
    }

    fn decision_face(decision: &str) -> (Color, Color) {
        let line = format!(
            "#9 {decision} open-files tier=L0 actor=did:key:z6MkOLD confirmed=true ts=1760518800000 — 已记下 @@ 打开文件"
        );
        let row = receipt_row(&line);
        let word = decision_label(decision);
        let mut found = None;
        walk(&row, &mut |node| {
            if let ComponentKind::Text { content } = &node.kind
                && content == &word
            {
                found = Some((node.style.background, node.style.color));
            }
        });
        found.expect("decision pill")
    }

    #[test]
    fn receipts_show_four_decisions_and_keep_the_written_subject() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 1);
        state::set_signal(RECEIPT_FILTER_SIGNAL, 0);
        state::set_text_signal(RECEIPT_QUERY_SIGNAL, "");
        state::set_text_signal(
            RECEIPT_SIGNAL,
            "#4 allowed open-files tier=L0 actor=did:key:z6MkOLD confirmed=true ts=1760518800000 — 已切换到文件 @@ 打开文件\n\
             #3 cancelled night-mode tier=L1 actor=did:key:z6MkOLD confirmed=false ts=1760515200000 — 你取消了，没有执行 @@ 夜间模式\n\
             #2 denied echo tier=L0 actor=did:key:z6MkOLD confirmed=true ts=1760511600000 — 当时还锁着 @@ 在终端运行 echo\n\
             #1 unrecognized summarize tier=L2 actor=did:key:z6MkOLD confirmed=false ts=1760508000000 — 没有对应命令 @@ 把这段总结一下",
        );
        state::set_text_signal(SUBJECT_SIGNAL, "did:key:z6MkNEWCURRENTSUBJECT");
        let viewer = build_receipts_viewer();
        let blob = text_blob(&viewer);
        for word in [
            "允许",
            "拒绝",
            "取消",
            "未识别",
            "打开文件",
            "夜间模式",
            "在终端运行 echo",
            "把这段总结一下",
        ] {
            assert!(blob.contains(word), "{word} missing from {blob}");
        }
        assert!(blob.contains("规则 L0"), "{blob}");
        assert!(blob.contains("同义 L1"), "{blob}");
        assert!(blob.contains("规划 L2"), "{blob}");
        assert!(blob.contains("结果 · 已切换到文件"), "{blob}");
        assert!(blob.contains("did:key:z6MkOLD"), "{blob}");
        assert!(blob.contains(':'));
        let labels = button_labels(&viewer);
        assert!(
            labels
                .iter()
                .any(|label| label.contains("did:key:z6Mk") && label.contains("SUBJECT")),
            "{labels:?}"
        );
        assert!(!labels.iter().any(|label| label.contains("z6MkOLD")));
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("删除") || label == "Delete")
        );
        assert_eq!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("did:key:z6MkOLD"),
            true
        );
        copy_subject();
        let copied = COPIED_SUBJECT.with(|slot| slot.borrow().clone());
        assert_eq!(copied, "did:key:z6MkNEWCURRENTSUBJECT");

        let allowed = decision_face("allowed");
        let denied = decision_face("denied");
        let cancelled = decision_face("cancelled");
        let unrecognized = decision_face("unrecognized");
        let faces = [allowed, denied, cancelled, unrecognized];
        for (index, face) in faces.iter().enumerate() {
            assert!(faces.iter().skip(index + 1).all(|other| other.0 != face.0));
            assert!(face.0.g > 20 || face.0.r > 20 || face.0.b > 20);
        }
        assert!(allowed.0.g > allowed.0.r);
        assert!(denied.0.r > denied.0.g);
        assert!(cancelled.0.r > cancelled.0.b);
        assert!(unrecognized.0.b > unrecognized.0.r);
        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(!format!("{viewer:?}").contains(hex), "{hex}");
        }

        on_shell_intent(OP_FILTER_DENIED, "");
        let filtered = text_blob(&build_receipts_viewer());
        assert!(filtered.contains("当时还锁着"), "{filtered}");
        assert!(filtered.contains("拒绝"), "{filtered}");
        assert!(!filtered.contains("打开文件"), "{filtered}");
        assert!(!filtered.contains("未识别"), "{filtered}");

        state::set_text_signal(RECEIPT_SIGNAL, "");
        state::set_signal(RECEIPT_FILTER_SIGNAL, 0);
        let empty = text_blob(&build_receipts_viewer());
        assert!(
            empty
                .contains("还没有收据。你确认并执行之后，每一条会出现在这里。取消和拒绝也会留下。"),
            "{empty}"
        );
        locale::set_locale("en");
        state::note_shell_viewport(1200.0, 800.0);
    }

    fn invoke_code(node: &Component) -> Option<u8> {
        match &node.on_click {
            EventAction::Invoke { code, .. } => Some(*code),
            _ => None,
        }
    }

    fn codes_for(root: &Component, label: &str) -> Vec<u8> {
        let mut codes = Vec::new();
        walk(root, &mut |node| {
            if let ComponentKind::Button { label: button } = &node.kind
                && button == label
                && let Some(code) = invoke_code(node)
            {
                codes.push(code);
            }
        });
        codes
    }

    #[test]
    fn lock_screen_covers_the_shell_and_the_open_lock_locks() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_text_signal(SUBJECT_SIGNAL, "did:key:z6MkhaXgExampleSubjectValue");
        let screen = build_lock_screen();
        let blob = text_blob(&screen);
        assert!(blob.contains("已锁定"), "{blob}");
        assert!(
            blob.contains("没有账号，也不走链。钥匙留在这台设备上。"),
            "{blob}"
        );
        assert!(blob.contains("用本机口令解锁"), "{blob}");
        assert!(
            blob.contains("解锁只影响这台设备。口令经 PBKDF2 校验后留在本机。"),
            "{blob}"
        );
        assert!(blob.contains('…'), "subject should be truncated: {blob}");
        assert!(
            !blob.contains("did:key:z6MkhaXgExampleSubjectValue"),
            "{blob}"
        );
        let mut placeholder = String::new();
        walk(&screen, &mut |node| {
            if let ComponentKind::TextInput {
                placeholder: hint,
                secure,
                ..
            } = &node.kind
            {
                assert!(*secure);
                placeholder = hint.clone();
            }
        });
        assert_eq!(placeholder, "本机口令");
        assert_eq!(codes_for(&screen, "用本机口令解锁"), vec![OP_UNLOCK]);
        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(!format!("{screen:?}").contains(hex), "{hex}");
        }

        state::set_signal(UNLOCKED_SIGNAL, 1);
        let bar = build_floating_taskbar();
        let icons = icon_buttons(&bar);
        let open = icons
            .iter()
            .find(|icon| icon.label == "已解锁")
            .expect("unlocked tray control");
        assert!(
            open.svg.contains("6.4-1.9"),
            "tray should show an open lock: {}",
            open.svg
        );
        assert_eq!(codes_for(&bar, "已解锁"), vec![OP_LOCK]);
        assert!(icons.iter().all(|icon| icon.label != "解锁"));
        assert!(!text_blob(&bar).contains("占位已解锁"));

        state::note_shell_viewport(390.0, 760.0);
        let phone = build_phone_home();
        assert!(
            icon_buttons(&phone)
                .iter()
                .any(|icon| icon.label == "已解锁" && icon.svg.contains("6.4-1.9"))
        );
        assert_eq!(codes_for(&phone, "已解锁"), vec![OP_LOCK]);

        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        let locked_bar = build_floating_taskbar();
        assert_eq!(codes_for(&locked_bar, "已锁定"), vec![OP_LOCK]);
        assert!(codes_for(&locked_bar, "解锁").is_empty());

        let identity = Component::column(Style::default(), settings_identity_page());
        let identity_blob = text_blob(&identity);
        assert!(
            identity_blob.contains("通行证：尚未注册"),
            "{identity_blob}"
        );
        assert!(identity_blob.contains("注册本机通行证"), "{identity_blob}");
        assert!(identity_blob.contains("还不是 passkey"), "{identity_blob}");
        assert!(
            !identity_blob.contains("WebAuthn 仪式还没做"),
            "{identity_blob}"
        );
        assert!(!identity_blob.contains("占位已解锁"), "{identity_blob}");
        assert_eq!(codes_for(&identity, "注册本机通行证"), vec![OP_ENROLL]);
        assert!(codes_for(&identity, "解锁").is_empty());

        state::set_signal(UNLOCKED_SIGNAL, 1);
        let unlocked_identity = Component::column(Style::default(), settings_identity_page());
        assert_eq!(codes_for(&unlocked_identity, "锁定"), vec![OP_LOCK]);
        assert!(text_blob(&unlocked_identity).contains("通行证：尚未注册"));
        let tasks = text_blob(&build_task_center());
        assert!(tasks.contains("已解锁"), "{tasks}");
        assert!(!tasks.contains("占位已解锁"), "{tasks}");
        assert!(!tasks.contains("解锁才能执行"), "{tasks}");

        let _ = build_shell();
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(LAUNCHER_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        let root = build_shell();
        let mut covered = false;
        walk(&root, &mut |node| {
            if node.style.z_index == CHROME_Z + 4
                && px(node.style.width) == 1200.0
                && px(node.style.height) == 800.0
            {
                covered = true;
            }
        });
        assert!(covered, "lock screen should cover the viewport");
        assert!(text_blob(&root).contains("已锁定"));

        locale::set_locale("en");
        assert_eq!(locale::t("passkey-unregistered"), "Passkey: not registered");
        assert_eq!(locale::t("passkey-enroll"), "Register a device passkey");
        assert_eq!(
            locale::t("lock-body"),
            "No account and no chain. The key stays on this device."
        );
        assert_eq!(
            locale::t("lock-footnote"),
            "Unlock only affects this device. The passphrase is checked with PBKDF2 and stays on this device."
        );
        assert_eq!(locale::t("lock-first"), "Unlock first");
        assert_eq!(locale::t("unlocked"), "Unlocked");
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
    }

    #[test]
    #[test]
    fn chain_sheet_names_the_saved_endpoint_and_does_not_broadcast() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::note_pointer_coarse(false);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(CHAIN_SIGNAL, 0);
        show_chain_read();
        assert_eq!(state::get_signal(CHAIN_SIGNAL), 1);
        assert_eq!(
            state::get_signal_text(CHAIN_RPC_SIGNAL),
            "http://127.0.0.1:8545"
        );
        let sheet = build_chain_sheet();
        let blob = text_blob(&sheet);
        assert!(blob.contains("允许读取这一次"), "{blob}");
        assert!(blob.contains("读取本机地址的余额"), "{blob}");
        assert!(blob.contains("http://127.0.0.1:8545"), "{blob}");
        assert!(blob.contains("不会广播"), "{blob}");
        assert!(!blob.contains("infura"), "{blob}");
        assert!(!blob.contains("alchemy"), "{blob}");
        assert_eq!(codes_for(&sheet, "允许这一次"), vec![OP_CHAIN_ONCE]);
        assert_eq!(codes_for(&sheet, "拒绝"), vec![OP_CHAIN_DENY]);

        state::note_shell_viewport(390.0, 760.0);
        state::note_pointer_coarse(true);
        let phone = build_chain_sheet();
        let (_, _, _, height) = button_face(&phone, "允许这一次");
        assert!(height >= 48.0, "{height}");
        locale::set_locale("en");
        let english = text_blob(&build_chain_sheet());
        assert!(english.contains("Allow this read"), "{english}");
        assert!(english.contains("http://127.0.0.1:8545"), "{english}");
        assert!(
            english.contains("Nothing is signed or broadcast"),
            "{english}"
        );
        locale::set_locale("zh-CN");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(CHAIN_SIGNAL, 0);
    }

    #[test]
    fn tx_sheet_decodes_fields_and_broadcast_is_a_second_confirm() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        sign_host::reset();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-tx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::note_pointer_coarse(false);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(SIGN_SIGNAL, 0);
        state::set_signal(BROADCAST_SIGNAL, 0);
        state::set_signal(CHAIN_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_text_signal(CHAIN_RPC_SIGNAL, "http://127.0.0.1:8545");

        on_shell_intent(OP_UNLOCK, "device-pass");
        with_session(open_tx_permission);
        assert_eq!(state::get_signal(SIGN_SIGNAL), 1);
        assert_eq!(state::get_signal(BROADCAST_SIGNAL), 0);
        let sheet = build_sign_sheet();
        let blob = text_blob(&sheet);
        assert!(blob.contains("签署这笔交易"), "{blob}");
        assert!(
            blob.contains("收款 0x1111111111111111111111111111111111111111"),
            "{blob}"
        );
        assert!(blob.contains("金额 1 wei"), "{blob}");
        assert!(blob.contains("nonce 7"), "{blob}");
        assert!(blob.contains("这一步不会广播"), "{blob}");
        assert!(!blob.contains("广播这一次"), "{blob}");
        assert!(!long_hex(&blob), "{blob}");
        assert_eq!(codes_for(&sheet, "签这一次"), vec![OP_SIGN_ONCE]);
        assert_eq!(codes_for(&sheet, "不签"), vec![OP_SIGN_DENY]);

        state::note_shell_viewport(390.0, 760.0);
        state::note_pointer_coarse(true);
        let phone = build_sign_sheet();
        let (_, _, _, height) = button_face(&phone, "签这一次");
        assert!(height >= 48.0, "{height}");

        on_shell_intent(OP_SIGN_DENY, "");
        locale::set_locale("en");
        state::note_shell_viewport(1200.0, 800.0);
        state::note_pointer_coarse(false);
        with_session(open_tx_permission);
        let english = text_blob(&build_sign_sheet());
        assert!(english.contains("Sign this transaction"), "{english}");
        assert!(
            english.contains("To 0x1111111111111111111111111111111111111111"),
            "{english}"
        );
        assert!(english.contains("Amount 1 wei"), "{english}");
        assert!(
            english.contains("This step does not broadcast"),
            "{english}"
        );
        assert_eq!(
            codes_for(&build_sign_sheet(), "Sign once"),
            vec![OP_SIGN_ONCE]
        );
        on_shell_intent(OP_SIGN_DENY, "");

        locale::set_locale("zh-CN");
        with_session(open_tx_permission);
        on_shell_intent(OP_SIGN_ONCE, "");
        assert_eq!(state::get_signal(SIGN_SIGNAL), 0);
        assert_eq!(state::get_signal(BROADCAST_SIGNAL), 1);
        let signed = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(signed.contains("wallet_sign_tx"), "{signed}");
        assert!(signed.contains("allowed"), "{signed}");
        assert!(signed.contains("signed=1"), "{signed}");
        assert!(signed.contains("还没有广播"), "{signed}");
        assert!(!long_hex(&signed), "{signed}");
        let broadcast = text_blob(&build_broadcast_sheet());
        assert!(broadcast.contains("把刚签的这笔发出去"), "{broadcast}");
        assert!(
            broadcast.contains("收款 0x1111111111111111111111111111111111111111"),
            "{broadcast}"
        );
        assert!(broadcast.contains("http://127.0.0.1:8545"), "{broadcast}");
        assert!(broadcast.contains("另一次确认"), "{broadcast}");
        assert!(!long_hex(&broadcast), "{broadcast}");
        assert_eq!(
            codes_for(&build_broadcast_sheet(), "不广播"),
            vec![OP_TX_HOLD]
        );
        assert_eq!(
            codes_for(&build_broadcast_sheet(), "广播这一次"),
            vec![OP_TX_SEND]
        );

        state::note_shell_viewport(390.0, 760.0);
        state::note_pointer_coarse(true);
        let phone_send = build_broadcast_sheet();
        let (_, _, _, send_h) = button_face(&phone_send, "广播这一次");
        assert!(send_h >= 48.0, "{send_h}");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);

        state::set_text_signal(CHAIN_RPC_SIGNAL, "");
        on_shell_intent(OP_TX_SEND, "");
        assert_eq!(state::get_signal(BROADCAST_SIGNAL), 0);
        let missed = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(missed.contains("chain_broadcast"), "{missed}");
        assert!(missed.contains("denied"), "{missed}");
        assert!(missed.contains("还没有节点"), "{missed}");
        assert!(!long_hex(&missed), "{missed}");

        state::set_text_signal(CHAIN_RPC_SIGNAL, "http://127.0.0.1:8545");
        with_session(open_tx_permission);
        on_shell_intent(OP_SIGN_ONCE, "");
        assert_eq!(state::get_signal(BROADCAST_SIGNAL), 1);
        on_shell_intent(OP_TX_HOLD, "");
        assert_eq!(state::get_signal(BROADCAST_SIGNAL), 0);
        let held = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(held.contains("cancelled"), "{held}");
        assert!(held.contains("没有广播"), "{held}");
        assert!(!long_hex(&held), "{held}");

        locale::set_locale("en");
        with_session(open_tx_permission);
        on_shell_intent(OP_SIGN_ONCE, "");
        let english_send = text_blob(&build_broadcast_sheet());
        assert!(
            english_send.contains("Send the transaction just signed"),
            "{english_send}"
        );
        assert!(
            english_send.contains("Endpoint http://127.0.0.1:8545"),
            "{english_send}"
        );
        assert!(english_send.contains("separate confirm"), "{english_send}");
        assert_eq!(
            codes_for(&build_broadcast_sheet(), "Don't broadcast"),
            vec![OP_TX_HOLD]
        );
        on_shell_intent(OP_TX_HOLD, "");

        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(SIGN_SIGNAL, 0);
        state::set_signal(BROADCAST_SIGNAL, 0);
        sign_host::reset();
        drop(isolated);
    }

    fn long_hex(text: &str) -> bool {
        let mut run = 0;
        for byte in text.bytes() {
            if byte.is_ascii_hexdigit() {
                run += 1;
                if run >= 70 {
                    return true;
                }
            } else {
                run = 0;
            }
        }
        false
    }

    fn passkey_binds_the_device_did_and_keeps_the_pairing_fingerprint() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-passkey-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        let seed_before = handoff::device_seed();
        let fingerprint_before = handoff::local_fingerprint();
        on_shell_intent(OP_UNLOCK, "device-pass");
        on_shell_intent(OP_ENROLL, "");
        assert_eq!(handoff::device_seed(), seed_before);
        assert_eq!(handoff::local_fingerprint(), fingerprint_before);
        let device = w3cos_p2p::DeviceIdentity::from_seed(seed_before);
        assert_eq!(state::get_signal_text(SUBJECT_SIGNAL), device.did_key());
        assert_ne!(device.did_key(), fingerprint_before);
        assert_eq!(state::get_signal_text(ENROLL_SIGNAL), "verified");
        let toast = state::get_signal_text(TOAST_SIGNAL);
        assert!(toast.contains("已核对断言"), "{toast}");
        on_shell_intent(OP_SETTINGS_IDENTITY, "");
        let page = text_blob(&build_settings_app());
        assert!(page.contains("通行证已绑定这台设备的 did:key"), "{page}");
        assert!(page.contains("配对指纹"), "{page}");
        assert!(!page.contains("PBKDF2"), "{page}");
        assert!(!page.contains(&device.did_key()), "{page}");
        assert!(!page.contains(&fingerprint_before), "{page}");

        state::note_shell_viewport(390.0, 760.0);
        state::note_pointer_coarse(true);
        let phone = build_settings_app();
        let (_, _, _, height) = button_face(&phone, "注册本机通行证");
        assert!(height >= 48.0, "{height}");
        let phone_blob = text_blob(&phone);
        assert!(phone_blob.contains("配对指纹"), "{phone_blob}");
        assert!(phone_blob.contains("断言已核对"), "{phone_blob}");

        locale::set_locale("en");
        let english = text_blob(&Component::column(
            Style::default(),
            settings_identity_page(),
        ));
        assert!(
            english.contains("Passkey is bound to this device's did:key"),
            "{english}"
        );
        assert!(english.contains("Pairing fingerprint"), "{english}");
        assert!(!english.contains("PBKDF2"), "{english}");
        locale::set_locale("zh-CN");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    struct IsolatedSession {
        session: Option<IntentSession>,
        booted: bool,
        receipt_dir: Option<std::ffi::OsString>,
        lexicon_path: Option<std::ffi::OsString>,
        dir: std::path::PathBuf,
    }

    impl Drop for IsolatedSession {
        fn drop(&mut self) {
            SESSION.with(|slot| {
                *slot.borrow_mut() = self.session.take();
            });
            let booted = self.booted;
            SESSION_BOOTED.with(|flag| flag.set(booted));
            restore_env("W3COS_RECEIPT_DIR", self.receipt_dir.take());
            restore_env("W3COS_LEXICON_PATH", self.lexicon_path.take());
            let _ = std::fs::remove_dir_all(&self.dir);
            lexicon::lock();
        }
    }

    fn restore_env(key: &str, previous: Option<std::ffi::OsString>) {
        match previous {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn passphrase_refuses_empty_and_wrong_and_lock_blocks_confirm() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(NIGHT_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);

        on_shell_intent(OP_UNLOCK, "");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 0);
        assert!(
            state::get_signal_text(TOAST_SIGNAL).contains("口令为空或不匹配"),
            "{}",
            state::get_signal_text(TOAST_SIGNAL)
        );
        on_shell_intent(OP_UNLOCK, "   ");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 0);

        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);
        on_shell_intent(OP_LOCK, "");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 0);

        on_shell_intent(OP_UNLOCK, "wrong-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 0);
        assert!(
            state::get_signal_text(TOAST_SIGNAL).contains("口令为空或不匹配"),
            "{}",
            state::get_signal_text(TOAST_SIGNAL)
        );

        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);

        on_shell_intent(OP_PROPOSE, "打开文件");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 1);
        let app_before = state::get_signal(APP_SIGNAL);
        let summary = state::get_signal_text(SUMMARY_SIGNAL);
        assert!(!summary.is_empty(), "confirm card keeps its summary");
        on_shell_intent(OP_LOCK, "");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 0);
        assert_eq!(state::get_signal(PHASE_SIGNAL), 1);
        assert_eq!(state::get_signal_text(SUMMARY_SIGNAL), summary);
        let dialog = build_confirm_dialog();
        let labels = button_labels(&dialog);
        assert!(labels.iter().any(|label| label == "先解锁"), "{labels:?}");
        assert!(!labels.iter().any(|label| label == "确认"), "{labels:?}");
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(state::get_signal(APP_SIGNAL), app_before);
        assert_eq!(state::get_signal(NIGHT_SIGNAL), 0);
        assert_eq!(state::get_signal(PHASE_SIGNAL), 1);

        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(
            state::get_signal(APP_SIGNAL),
            app_before,
            "a voided proposal does not run after unlock"
        );

        locale::set_locale("en");
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    fn initial_focus(root: &Component) -> Vec<String> {
        let mut labels = Vec::new();
        walk(root, &mut |node| {
            let marked = node.style.custom_properties.as_ref().is_some_and(|props| {
                props
                    .get("--w3cos-initial-focus")
                    .is_some_and(|value| value == "1")
            });
            if marked && let ComponentKind::Button { label } = &node.kind {
                labels.push(label.clone());
            }
        });
        labels
    }

    fn clicks_disabled(root: &Component, label: &str) -> bool {
        let mut disabled = false;
        walk(root, &mut |node| {
            if let ComponentKind::Button { label: button } = &node.kind
                && button == label
                && matches!(node.on_click, EventAction::None)
            {
                disabled = true;
            }
        });
        disabled
    }

    #[test]
    fn confirm_does_not_grant_process_and_permission_is_once() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-perm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(NIGHT_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_text_signal(TERMINAL_SIGNAL, "");
        state::set_text_signal(TERM_OUT_SIGNAL, "");
        capability_gate::deny(shell_tools::CAP_PROCESS);

        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);

        on_shell_intent(OP_PROPOSE, "打开终端");
        assert_eq!(state::get_signal_text(SUMMARY_SIGNAL), "打开终端");
        assert!(
            state::get_signal_text(IMPACT_SIGNAL).contains("切换到终端窗口"),
            "{}",
            state::get_signal_text(IMPACT_SIGNAL)
        );
        let dialog = build_confirm_dialog();
        let blob = text_blob(&dialog);
        assert!(blob.contains("规则 L0"), "{blob}");
        assert!(blob.contains("打开终端"), "{blob}");
        assert!(blob.contains("确认后才会执行，并写下一张收据。"), "{blob}");
        assert!(!blob.contains("请确认"), "{blob}");
        assert_eq!(
            button_labels(&dialog)
                .into_iter()
                .filter(|label| label == "取消" || label == "确认")
                .collect::<Vec<_>>(),
            vec!["取消".to_string(), "确认".to_string()]
        );
        assert_eq!(initial_focus(&dialog), vec!["确认".to_string()]);
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(state::get_signal(APP_SIGNAL), 2);
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert!(state::get_signal_text(TERM_OUT_SIGNAL).is_empty());
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));

        state::set_text_signal(TERMINAL_SIGNAL, "echo w3cos-once");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        assert!(state::get_signal_text(TERM_OUT_SIGNAL).is_empty());
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));
        let sheet = build_permission_sheet();
        let perm = text_blob(&sheet);
        assert!(perm.contains("允许运行这一次 echo"), "{perm}");
        assert!(
            perm.contains("这不是刚才那个确认。打开终端已经完成。现在要单独授权进程，用完即收回。"),
            "{perm}"
        );
        assert!(
            perm.contains("只放行 echo、pwd、help。没有系统 shell。"),
            "{perm}"
        );
        assert!(perm.contains("拒绝也会写收据。身份必须已解锁。"), "{perm}");
        assert_eq!(
            button_labels(&sheet)
                .into_iter()
                .filter(|label| label == "拒绝" || label == "仅此一次")
                .collect::<Vec<_>>(),
            vec!["拒绝".to_string(), "仅此一次".to_string()]
        );
        assert!(
            !button_labels(&sheet).iter().any(|label| label == "确认"),
            "{perm}"
        );
        assert_eq!(initial_focus(&sheet), vec!["拒绝".to_string()]);
        assert_ne!(
            dialog.style.background, sheet.style.background,
            "the permission sheet should not reuse the confirm card"
        );
        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(!format!("{dialog:?}").contains(hex), "{hex}");
            assert!(!format!("{sheet:?}").contains(hex), "{hex}");
        }

        on_shell_intent(OP_PERM_DENY, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert!(state::get_signal_text(TERM_OUT_SIGNAL).is_empty());
        let denied = state::get_signal_text(RECEIPT_SIGNAL);
        assert!(
            denied.contains("Denied") && denied.contains("refused"),
            "{denied}"
        );

        on_shell_intent(OP_RUN_TERMINAL, "");
        on_shell_intent(OP_PERM_ONCE, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));
        let ran = state::get_signal_text(TERM_OUT_SIGNAL);
        assert!(ran.contains("w3cos-once"), "{ran}");
        assert!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("allowed"),
            "{}",
            state::get_signal_text(RECEIPT_SIGNAL)
        );

        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        assert_eq!(state::get_signal_text(TERM_OUT_SIGNAL), ran);
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));

        on_shell_intent(OP_LOCK, "");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 0);
        let locked = build_permission_sheet();
        let locked_blob = text_blob(&locked);
        assert!(
            locked_blob.contains("先解锁。这次不会运行。"),
            "{locked_blob}"
        );
        assert!(
            locked_blob.contains("拒绝也会写收据。身份必须已解锁。"),
            "{locked_blob}"
        );
        assert!(clicks_disabled(&locked, "拒绝"));
        assert!(clicks_disabled(&locked, "仅此一次"));
        on_shell_intent(OP_PERM_ONCE, "");
        assert_eq!(state::get_signal_text(TERM_OUT_SIGNAL), ran);
        assert!(!capability_gate::is_granted(shell_tools::CAP_PROCESS));
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        on_shell_intent(OP_PERM_DENY, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);

        on_shell_intent(OP_UNLOCK, "device-pass");
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(NIGHT_SIGNAL, 0);
        on_shell_intent(OP_PROPOSE, "夜间模式");
        assert_eq!(state::get_signal_text(SUMMARY_SIGNAL), "打开夜间模式");
        assert_eq!(
            initial_focus(&build_confirm_dialog()),
            vec!["取消".to_string()]
        );
        on_shell_intent(OP_CANCEL, "");
        on_shell_intent(OP_PROPOSE, "file manager");
        assert_eq!(state::get_signal_text(TIER_SIGNAL), "L1");
        assert_eq!(state::get_signal_text(SUMMARY_SIGNAL), "打开文件");
        let synonym = text_blob(&build_confirm_dialog());
        assert!(synonym.contains("同义 L1"), "{synonym}");
        assert!(
            synonym.contains("这句话没有对上规则表，本机把它当成同一件事。"),
            "{synonym}"
        );
        assert_eq!(
            initial_focus(&build_confirm_dialog()),
            vec!["确认".to_string()]
        );

        state::note_shell_viewport(390.0, 760.0);
        let phone = build_confirm_dialog();
        walk(&phone, &mut |node| {
            if let ComponentKind::Button { label } = &node.kind
                && (label == "取消" || label == "确认")
            {
                assert_eq!(px(node.style.height), 48.0, "{label}");
            }
        });
        let phone_perm = build_permission_sheet();
        walk(&phone_perm, &mut |node| {
            if let ComponentKind::Button { label } = &node.kind
                && (label == "拒绝" || label == "仅此一次")
            {
                assert_eq!(px(node.style.height), 48.0, "{label}");
            }
        });

        locale::set_locale("en");
        assert_eq!(locale::t("perm-deny"), "Deny");
        assert_eq!(
            locale::t("perm-footnote"),
            "A denial is written on the receipt too. Identity has to be unlocked."
        );
        assert_eq!(
            locale::t("l1-same"),
            "That sentence missed the rule table. This device treated it as the same action."
        );
        capability_gate::deny(shell_tools::CAP_PROCESS);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    fn prop(node: &Component, key: &str) -> Option<String> {
        node.style
            .custom_properties
            .as_ref()
            .and_then(|props| props.get(key).cloned())
    }

    fn button_face(
        root: &Component,
        label: &str,
    ) -> (Option<u8>, Option<String>, Option<String>, f32) {
        let mut found = None;
        walk(root, &mut |node| {
            if found.is_some() {
                return;
            }
            if let ComponentKind::Button { label: current } = &node.kind
                && current == label
            {
                found = Some((
                    invoke_code(node),
                    prop(node, "--w3cos-selected"),
                    prop(node, "--w3cos-switch"),
                    match node.style.min_height {
                        Dimension::Px(value) => value,
                        _ => 0.0,
                    },
                ));
            }
        });
        found.unwrap_or_else(|| panic!("missing button {label}"))
    }

    #[test]
    fn settings_sidebar_switches_panes_and_toggles_night_and_context() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(NIGHT_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(SETTINGS_MENU_SIGNAL, 0);
        state::set_text_signal(PENDING_LOCALE_SIGNAL, "");
        on_shell_intent(OP_UNLOCK, "device-pass");
        assert_eq!(state::get_signal(UNLOCKED_SIGNAL), 1);

        on_shell_intent(OP_SETTINGS_LOCAL, "");
        let desktop = build_settings_app();
        let nav = ["本机", "身份", "语言", "代理能力", "设备与接续"];
        for label in nav {
            assert!(
                button_labels(&desktop).iter().any(|item| item == label),
                "{label} missing from {}",
                text_blob(&desktop)
            );
        }
        assert!(!button_labels(&desktop).iter().any(|item| item == "权限"));
        assert!(!button_labels(&desktop).iter().any(|item| item == "L2 模型"));
        let (local_code, local_selected, _, _) = button_face(&desktop, "本机");
        assert_eq!(local_selected.as_deref(), Some("1"));
        assert_eq!(local_code, Some(OP_SETTINGS_LOCAL));
        let (night_code, _, night_switch, _) = button_face(&desktop, "夜间模式");
        assert_eq!(night_switch.as_deref(), Some("0"));
        assert_eq!(night_code, Some(OP_CHIP_NIGHT));
        let local_blob = text_blob(&desktop);
        assert!(!local_blob.lines().any(|line| line == "开" || line == "关"));

        on_shell_intent(OP_CHIP_NIGHT, "");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 1);
        assert_eq!(state::get_signal(NIGHT_SIGNAL), 0);
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(state::get_signal(NIGHT_SIGNAL), 1);
        on_shell_intent(OP_CHIP_NIGHT, "");
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(state::get_signal(NIGHT_SIGNAL), 0);
        state::set_signal(NIGHT_SIGNAL, 1);
        let (_, _, night_on, _) = button_face(&build_settings_app(), "夜间模式");
        assert_eq!(night_on.as_deref(), Some("1"));
        state::set_signal(NIGHT_SIGNAL, 0);

        state::set_text_signal(SUBJECT_SIGNAL, "did:key:z6MkhaXgExampleSubjectValue");
        on_shell_intent(OP_SETTINGS_IDENTITY, "");
        let identity = build_settings_app();
        let (_, identity_selected, _, _) = button_face(&identity, "身份");
        assert_eq!(identity_selected.as_deref(), Some("1"));
        let (_, local_selected, _, _) = button_face(&identity, "本机");
        assert_eq!(local_selected, None);
        let identity_blob = text_blob(&identity);
        assert!(
            identity_blob.contains("通行证：尚未注册"),
            "{identity_blob}"
        );
        assert!(identity_blob.contains("注册本机通行证"), "{identity_blob}");
        assert!(identity_blob.contains('…'), "{identity_blob}");
        assert!(
            !identity_blob.contains("did:key:z6MkhaXgExampleSubjectValue"),
            "{identity_blob}"
        );
        assert!(
            !identity_blob.contains("WebAuthn 仪式还没做"),
            "{identity_blob}"
        );
        assert_eq!(codes_for(&identity, "复制"), vec![OP_COPY_SUBJECT]);
        assert_eq!(codes_for(&identity, "注册本机通行证"), vec![OP_ENROLL]);

        on_shell_intent(OP_SETTINGS_LANGUAGE, "");
        let language = text_blob(&build_settings_app());
        assert!(language.contains("简体中文"), "{language}");
        assert!(language.contains("English"), "{language}");
        assert!(language.contains("zh-CN"), "{language}");
        assert!(language.contains("en"), "{language}");
        on_shell_intent(OP_LOCALE_EN, "");
        assert_eq!(locale::current(), "zh-CN");
        assert_eq!(state::get_signal_text(PENDING_LOCALE_SIGNAL), "en");
        let pending = text_blob(&build_settings_app());
        assert!(pending.contains("待确认"), "{pending}");
        let card = build_locale_confirm();
        let card_blob = text_blob(&card);
        assert!(card_blob.contains("规则 L0"), "{card_blob}");
        assert!(card_blob.contains("把界面改成 English"), "{card_blob}");
        assert!(
            card_blob
                .contains("只换界面上的句子。已经写下的收据保持原样，路径和时间格式跟 English。"),
            "{card_blob}"
        );
        on_shell_intent(OP_CANCEL, "");
        assert_eq!(locale::current(), "zh-CN");
        assert!(state::get_signal_text(PENDING_LOCALE_SIGNAL).is_empty());
        on_shell_intent(OP_LOCALE_EN, "");
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(locale::current(), "en");
        assert!(
            state::get_signal_text(RECEIPT_SIGNAL).contains("set-locale"),
            "{}",
            state::get_signal_text(RECEIPT_SIGNAL)
        );
        on_shell_intent(OP_LOCALE_ZH, "");
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(locale::current(), "zh-CN");

        on_shell_intent(OP_RESTORE, "");
        on_shell_intent(OP_SETTINGS_BRIDGE, "");
        let agent = build_settings_app();
        let agent_blob = text_blob(&agent);
        assert!(agent_blob.contains("看见当前窗口"), "{agent_blob}");
        assert!(
            agent_blob.contains("这和跨设备同步是两件事。"),
            "{agent_blob}"
        );
        assert!(!agent_blob.contains('×'), "{agent_blob}");
        assert!(!text_blob(&build_intent_panel()).contains('×'));
        assert!(!text_blob(&build_floating_taskbar()).contains('×'));
        let (share_code, _, share_on, _) = button_face(&agent, "看见当前窗口");
        assert_eq!(share_on.as_deref(), Some("1"));
        assert_eq!(share_code, Some(OP_REVOKE));
        on_shell_intent(OP_REVOKE, "");
        assert_eq!(state::get_signal(CONTEXT_SIGNAL), 0);
        let hidden = build_settings_app();
        let (hidden_code, _, hidden_on, _) = button_face(&hidden, "看见当前窗口");
        assert_eq!(hidden_on.as_deref(), Some("0"));
        assert_eq!(hidden_code, Some(OP_RESTORE));
        on_shell_intent(OP_RESTORE, "");
        assert_eq!(state::get_signal(CONTEXT_SIGNAL), 1);

        state::note_shell_viewport(390.0, 760.0);
        state::set_signal(SETTINGS_MENU_SIGNAL, 1);
        let stack = build_settings_app();
        for label in nav {
            let (_, _, _, height) = button_face(&stack, label);
            assert!(height >= 48.0, "{label}");
        }
        let mut narrow_sidebar = false;
        walk(&stack, &mut |node| {
            if matches!(node.style.width, Dimension::Px(width) if (width - 108.0).abs() < 0.5) {
                narrow_sidebar = true;
            }
        });
        assert!(!narrow_sidebar);
        on_shell_intent(OP_SETTINGS_IDENTITY, "");
        assert_eq!(state::get_signal(SETTINGS_MENU_SIGNAL), 0);
        let pane = build_settings_app();
        assert!(text_blob(&pane).contains("通行证：尚未注册"));
        assert_eq!(codes_for(&pane, "返回"), vec![OP_SETTINGS_BACK]);
        on_shell_intent(OP_SETTINGS_BACK, "");
        assert_eq!(state::get_signal(SETTINGS_MENU_SIGNAL), 1);

        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(
                !format!("{:?}", build_settings_app()).contains(hex),
                "{hex}"
            );
        }
        locale::set_locale("en");
        assert_eq!(locale::t("settings-agent"), "Agent");
        assert_eq!(
            locale::t("settings-context-switch"),
            "See the current window"
        );
        assert_eq!(
            locale::t_args("locale-confirm-title", &[("name", "English")]),
            "Change the interface to English"
        );
        locale::set_locale("zh-CN");
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(NIGHT_SIGNAL, 0);
        state::set_signal(SETTINGS_MENU_SIGNAL, 1);
        state::set_text_signal(PENDING_LOCALE_SIGNAL, "");
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    fn clock_cells(root: &Component) -> String {
        let mut cells = String::new();
        walk(root, &mut |node| {
            if let ComponentKind::Text { content } = &node.kind
                && node.style.position != Position::Absolute
                && content.chars().count() == 1
                && (content == ":" || content.chars().all(|ch| ch.is_ascii_digit()))
            {
                cells.push_str(content);
            }
        });
        cells
    }

    fn percent_texts(root: &Component) -> Vec<String> {
        let mut found = Vec::new();
        walk(root, &mut |node| {
            if let ComponentKind::Text { content } = &node.kind
                && content.ends_with('%')
            {
                found.push(content.clone());
            }
        });
        found
    }

    fn assert_no_fake_desktop(name: &str, root: &Component) {
        let blob = text_blob(root);
        for banned in [
            "助手",
            "处理器",
            "内存",
            "存储",
            "网络",
            "在线",
            "Online",
            "CPU",
            "Memory",
            "Storage",
            "Network",
            "23%",
            "5.4",
            "8 GB",
            "14:32",
            "W3C Apps",
            "会话在旁边的任务中心",
            "The live session stays",
        ] {
            assert!(
                !blob.contains(banned),
                "{name} still shows {banned}: {blob}"
            );
        }
        let labels = button_labels(root);
        assert!(
            !labels
                .iter()
                .any(|label| label == "助手" || label == "Agent" || label == "打开助手"),
            "{name} still has an assistant entry: {labels:?}"
        );
        let percents = percent_texts(root);
        match live_battery() {
            Some(reading) => assert_eq!(percents, vec![reading], "{name}"),
            None => assert!(percents.is_empty(), "{name} invented {percents:?}"),
        }
        for hex in ["5b4fe0", "6c5ce7", "9b59b6"] {
            assert!(!format!("{root:?}").contains(hex), "{name} uses {hex}");
        }
    }

    #[test]
    fn desktop_has_no_assistant_app_and_no_fake_metrics() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        let dir = std::env::temp_dir().join(format!(
            "w3cos-honesty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_signal(MINIMIZED_MASK_SIGNAL, 0);
        state::set_signal(TASKS_SIGNAL, 0);
        state::set_signal(LAUNCHER_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_text_signal(LAUNCHER_QUERY_SIGNAL, "");
        state::set_signal(SETTINGS_TAB_SIGNAL, 0);
        state::set_signal(SETTINGS_MENU_SIGNAL, 0);

        let desktop = build_desktop();
        assert_eq!(desktop.children.len(), 1);
        assert_no_fake_desktop("desktop", &desktop);
        let icons = text_blob(&desktop);
        for name in ["文件", "终端", "设置", "浏览器", "编辑器"] {
            assert!(icons.contains(name), "{name} missing from {icons}");
        }

        let bar = build_floating_taskbar();
        assert_no_fake_desktop("taskbar", &bar);
        assert_eq!(codes_for(&bar, "任务"), vec![OP_TOGGLE_TASKS]);
        assert_eq!(clock_cells(&bar), live_clock());

        state::set_signal(LAUNCHER_SIGNAL, 1);
        let launcher = build_launcher();
        assert_no_fake_desktop("launcher", &launcher);
        state::set_text_signal(LAUNCHER_QUERY_SIGNAL, "助手");
        let missed = build_launcher();
        assert!(text_blob(&missed).contains("没有匹配的应用"));
        assert!(
            !button_labels(&missed)
                .iter()
                .any(|label| label.contains("助手"))
        );
        state::set_text_signal(LAUNCHER_QUERY_SIGNAL, "");
        state::set_signal(LAUNCHER_SIGNAL, 0);

        state::note_shell_viewport(800.0, 600.0);
        assert_no_fake_desktop("tablet rail", &build_tablet_rail());
        assert_eq!(
            codes_for(&build_tablet_rail(), "任务"),
            vec![OP_TOGGLE_TASKS]
        );

        state::note_shell_viewport(390.0, 760.0);
        let phone = build_phone_home();
        assert_eq!(phone.children.len(), 2);
        assert_no_fake_desktop("phone", &phone);
        assert_eq!(codes_for(&phone, "任务"), vec![OP_TOGGLE_TASKS]);
        assert_eq!(clock_cells(&phone), live_clock());
        for name in ["文件", "终端", "设置", "浏览器", "编辑器"] {
            assert!(text_blob(&phone).contains(name), "{name}");
        }

        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(OPEN_MASK_SIGNAL, app_bit(4) | app_bit(1));
        state::set_signal(APP_SIGNAL, 4);
        let switcher = text_blob(&build_switcher());
        assert!(!switcher.contains("助手"), "{switcher}");
        assert!(switcher.contains("文件"), "{switcher}");
        cycle_windows();
        assert_eq!(state::get_signal(APP_SIGNAL), 1);
        assert_eq!(state::get_signal(TASKS_SIGNAL), 0);

        state::set_signal(OPEN_MASK_SIGNAL, app_bit(4));
        state::set_signal(APP_SIGNAL, 4);
        on_shell_intent(OP_FOCUS_AGENT, "");
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert_eq!(state::get_signal(OPEN_MASK_SIGNAL) & app_bit(4), 0);
        assert_eq!(state::get_signal(TASKS_SIGNAL), 1);
        let tasks = text_blob(&build_task_center());
        assert!(tasks.contains("任务中心"), "{tasks}");
        assert!(tasks.contains("收据"), "{tasks}");
        assert!(tasks.contains("收起任务"), "{tasks}");
        assert!(!tasks.contains("会话在旁边的任务中心"), "{tasks}");
        assert!(!text_blob(&build_stage()).contains("助手"));
        on_shell_intent(OP_TOGGLE_TASKS, "");
        assert_eq!(state::get_signal(TASKS_SIGNAL), 0);

        on_shell_intent(OP_UNLOCK, "device-pass");
        on_shell_intent(OP_PROPOSE, "打开助手");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 1);
        let confirm = text_blob(&build_confirm_dialog());
        assert!(confirm.contains("打开任务中心"), "{confirm}");
        assert!(confirm.contains("不新开应用窗口"), "{confirm}");
        assert!(!confirm.contains("助手"), "{confirm}");
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(state::get_signal(APP_SIGNAL), 0);
        assert_eq!(state::get_signal(OPEN_MASK_SIGNAL) & app_bit(4), 0);
        assert_eq!(state::get_signal(TASKS_SIGNAL), 1);
        assert!(!text_blob(&build_desktop()).contains("助手"));

        state::set_signal(TASKS_SIGNAL, 0);
        state::set_signal(SETTINGS_TAB_SIGNAL, 0);
        state::set_signal(SETTINGS_MENU_SIGNAL, 0);
        let local = text_blob(&build_settings_app());
        for banned in ["处理器", "23%", "5.4", "8 GB", "在线", "92%"] {
            assert!(!local.contains(banned), "本机 shows {banned}: {local}");
        }

        locale::set_locale("en");
        state::set_signal(TASKS_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_text_signal(LAUNCHER_QUERY_SIGNAL, "agent");
        let english = build_desktop();
        assert_no_fake_desktop("english desktop", &english);
        for name in ["Files", "Terminal", "Settings", "Browser", "Editor"] {
            assert!(text_blob(&english).contains(name), "{name}");
        }
        let english_launcher = build_launcher();
        assert!(text_blob(&english_launcher).contains("No matching app."));
        assert!(
            !button_labels(&english_launcher)
                .iter()
                .any(|label| label == "Agent" || label.contains("Agent"))
        );
        assert_eq!(
            codes_for(&build_floating_taskbar(), "Tasks"),
            vec![OP_TOGGLE_TASKS]
        );
        state::note_shell_viewport(390.0, 760.0);
        assert_no_fake_desktop("english phone", &build_phone_home());
        assert_eq!(
            locale::t("impact-tasks").contains("No application window"),
            true
        );

        locale::set_locale("zh-CN");
        state::set_signal(TASKS_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_text_signal(LAUNCHER_QUERY_SIGNAL, "");
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    fn reset_notices() {
        NOTICES.with(|slot| slot.borrow_mut().clear());
        SEEN_RECEIPTS.with(|slot| slot.borrow_mut().clear());
        RECEIPT_NOTICES_BASELINED.with(|flag| flag.set(false));
        NOTICE_FOCUS.with(|slot| slot.borrow_mut().clear());
        state::set_signal(NOTIFY_SIGNAL, 0);
        state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
    }

    #[test]
    fn notifications_start_empty_and_record_a_real_receipt() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        reset_notices();
        locale::set_locale("zh-CN");
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(NOTIFY_SIGNAL, 1);
        let empty = build_notify_panel();
        let empty_blob = text_blob(&empty);
        assert!(empty_blob.contains("通知"), "{empty_blob}");
        assert!(empty_blob.contains("没有新通知。"), "{empty_blob}");
        assert!(!empty_blob.contains("已打开"), "{empty_blob}");
        assert!(!empty_blob.contains("23%"), "{empty_blob}");
        let top = px(empty.style.top);
        let panel_h = px(empty.style.height);
        let floor = intent_block_top(800.0) - 12.0;
        assert!(
            top + panel_h <= floor + 0.5,
            "panel covers the intent bar: {top} + {panel_h} > {floor}"
        );
        assert!(px(empty.style.left) > 600.0, "panel is not on the right");
        state::set_signal(NOTIFY_SIGNAL, 0);
        assert_eq!(
            codes_for(&build_floating_taskbar(), "通知"),
            vec![OP_NOTIFY_TOGGLE]
        );
        assert!(!text_blob(&build_floating_taskbar()).contains("9+"));

        state::note_shell_viewport(390.0, 760.0);
        assert!(on_shell_swipe(180.0, 12.0, 2.0, 80.0));
        assert_eq!(state::get_signal(NOTIFY_SIGNAL), 1);
        let phone = build_notify_panel();
        assert_eq!(px(phone.style.top), 0.0);
        assert_eq!(px(phone.style.width), 390.0);
        assert!(px(phone.style.height) < 760.0 * 0.7);
        assert!(text_blob(&phone).contains("没有新通知。"));
        state::set_signal(NOTIFY_SIGNAL, 0);
        state::note_shell_viewport(1200.0, 800.0);

        let dir = std::env::temp_dir().join(format!(
            "w3cos-notify-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let isolated = IsolatedSession {
            session: SESSION.with(|slot| slot.borrow_mut().take()),
            booted: SESSION_BOOTED.with(|flag| flag.replace(false)),
            receipt_dir: std::env::var_os("W3COS_RECEIPT_DIR"),
            lexicon_path: std::env::var_os("W3COS_LEXICON_PATH"),
            dir: dir.clone(),
        };
        unsafe {
            std::env::set_var("W3COS_RECEIPT_DIR", &dir);
            std::env::set_var("W3COS_LEXICON_PATH", dir.join("lexicon.w3p1"));
        }
        reset_notices();
        on_shell_intent(OP_UNLOCK, "device-pass");
        on_shell_intent(OP_PROPOSE, "打开文件");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 1);
        let pending = text_blob(&build_notify_panel());
        assert!(
            !pending.contains("已打开文件"),
            "confirm must not arrive as a notification: {pending}"
        );
        assert!(
            !button_labels(&build_confirm_dialog())
                .iter()
                .any(|label| label == "通知")
        );
        state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
        on_shell_intent(OP_CONFIRM, "");
        assert_eq!(state::get_signal(PHASE_SIGNAL), 2);
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        let fed = text_blob(&build_notify_panel());
        assert!(fed.contains("已打开文件。"), "{fed}");
        assert!(fed.contains("收据 #"), "{fed}");
        assert!(fed.contains("规则 L0"), "{fed}");
        assert!(fed.contains("刚刚"), "{fed}");
        assert_eq!(state::get_signal(NOTIFY_UNREAD_SIGNAL), 1);
        let open_code = codes_for(&build_notify_panel(), "已打开文件。");
        assert_eq!(open_code, vec![OP_NOTIFY_OPEN]);
        assert!(!open_code.contains(&OP_CONFIRM));
        assert!(!open_code.contains(&OP_PERM_ONCE));
        state::set_signal(NOTIFY_SIGNAL, 1);
        state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
        let open_title = text_blob(&build_notify_panel());
        assert!(open_title.contains("通知"), "{open_title}");
        on_shell_intent(OP_NOTIFY_OPEN, "receipt:#1");
        assert_eq!(state::get_signal(RECEIPTS_OPEN_SIGNAL), 1);
        assert_eq!(state::get_signal(NOTIFY_SIGNAL), 0);
        assert_eq!(state::get_signal(RECEIPT_PAGE_SIGNAL), 0);
        let drawer = text_blob(&build_receipts_viewer());
        assert!(
            drawer.contains("#1") || drawer.contains("打开文件"),
            "{drawer}"
        );

        let mut lines = Vec::new();
        for id in 1..=7 {
            lines.push(format!(
                "#{id} allowed open:files tier=L0 actor=me confirmed=true ts=1 — Switched to Files. @@ 打开文件"
            ));
        }
        state::set_text_signal(RECEIPT_SIGNAL, &lines.join("\n"));
        on_shell_intent(OP_NOTIFY_OPEN, "receipt:#7");
        assert_eq!(state::get_signal(RECEIPT_PAGE_SIGNAL), 1);
        assert_eq!(state::get_signal(RECEIPTS_OPEN_SIGNAL), 1);

        state::set_signal(RECEIPTS_OPEN_SIGNAL, 0);
        on_shell_intent(OP_LOCK, "");
        let locked = text_blob(&build_notify_panel());
        assert!(locked.contains("身份已锁定。"), "{locked}");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);

        on_shell_intent(OP_UNLOCK, "device-pass");
        state::set_text_signal(TERMINAL_SIGNAL, "echo hi");
        on_shell_intent(OP_RUN_TERMINAL, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 1);
        let asking = text_blob(&build_notify_panel());
        assert!(
            !asking.contains("允许运行"),
            "permission stays on its sheet: {asking}"
        );
        on_shell_intent(OP_PERM_DENY, "");
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        let denied = text_blob(&build_notify_panel());
        assert!(denied.contains("没有运行 echo。"), "{denied}");
        assert!(denied.contains("收据已记下拒绝。"), "{denied}");

        let locked_line = "#9 denied l2:run tier=L0 actor=me confirmed=true ts=1 — Denied: local identity is locked. Nothing was changed. @@ echo";
        let locked_notice = notice_from_receipt(locked_line).unwrap();
        assert_eq!(locked_notice.title, "没有运行 echo。");
        assert!(locked_notice.detail.contains("当时还锁着"));

        locale::set_locale("en");
        assert_eq!(locale::t("notify-empty"), "No new notifications.");
        assert_eq!(
            locale::t_args("notify-opened", &[("name", "Files")]),
            "Opened Files."
        );
        assert_eq!(locale::t("notify-title"), "Notifications");

        reset_notices();
        locale::set_locale("zh-CN");
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_text_signal(TERMINAL_SIGNAL, "");
        state::note_shell_viewport(1200.0, 800.0);
        drop(isolated);
    }

    fn button_extent(root: &Component, label: &str) -> (f32, f32) {
        let mut found = None;
        walk(root, &mut |node| {
            if let ComponentKind::Button { label: button } = &node.kind
                && button == label
            {
                let width = match node.style.width {
                    Dimension::Px(value) => value,
                    _ => match node.style.min_width {
                        Dimension::Px(value) => value,
                        _ => 0.0,
                    },
                };
                let height = match node.style.height {
                    Dimension::Px(value) => value,
                    _ => 0.0,
                };
                let min_height = match node.style.min_height {
                    Dimension::Px(value) => value,
                    _ => 0.0,
                };
                found = Some((width, height.max(min_height)));
            }
        });
        found.unwrap_or_else(|| panic!("missing button {label}"))
    }

    fn focusable_names(root: &Component) -> Vec<String> {
        let mut names = Vec::new();
        walk(root, &mut |node| match &node.kind {
            ComponentKind::Button { label } if !label.trim().is_empty() => {
                names.push(label.clone());
            }
            ComponentKind::TextInput { placeholder, .. } => names.push(placeholder.clone()),
            _ => {}
        });
        names
    }

    #[test]
    fn dialogs_trap_focus_and_chrome_meets_the_touch_minimum() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);
        handoff::reset_for_tests();
        ime_bar::reset_for_tests();
        let _ = build_shell();

        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(OPEN_MASK_SIGNAL, app_bit(1));
        state::set_signal(MINIMIZED_MASK_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 1);
        state::set_signal(PHASE_SIGNAL, 1);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(NOTIFY_SIGNAL, 0);
        state::set_signal(LAUNCHER_SIGNAL, 0);
        state::set_signal(RECEIPTS_OPEN_SIGNAL, 0);
        state::set_signal(SWITCHER_SIGNAL, 0);
        state::set_signal(TASKS_SIGNAL, 0);
        state::set_text_signal(PENDING_LOCALE_SIGNAL, "");
        state::set_text_signal(TOAST_SIGNAL, "");
        state::set_text_signal(SUMMARY_SIGNAL, "打开文件");
        state::set_text_signal(IMPACT_SIGNAL, "会打开文件窗口。");
        state::set_text_signal(TIER_SIGNAL, "L0");
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "confirm");

        let cancel = locale::t("cancel");
        let confirm = locale::t("confirm");
        let dialog = build_confirm_dialog();
        assert!(button_extent(&dialog, &cancel).1 >= 44.0, "{}", cancel);
        assert!(button_extent(&dialog, &confirm).1 >= 44.0, "{}", confirm);
        let propose = locale::t("propose");
        let intent = build_intent_panel();
        let (propose_w, propose_h) = button_extent(&intent, &propose);
        assert!(
            propose_w >= 44.0 && propose_h >= 44.0,
            "{propose_w}x{propose_h}"
        );
        let nav = nav_button(
            &locale::t("settings-language-nav"),
            false,
            OP_SETTINGS_LANGUAGE,
        );
        assert!(button_extent(&nav, &locale::t("settings-language-nav")).1 >= 48.0);
        let notice = notice_row(&ShellNotice {
            title: "已关闭 文件。".into(),
            detail: String::new(),
            draft: "receipt:#1".into(),
        });
        assert!(button_extent(&notice, "已关闭 文件。").1 >= 48.0);
        let bar = build_floating_taskbar();
        let icons = icon_buttons(&bar);
        assert!(!icons.is_empty());
        assert!(
            icons.iter().all(|icon| icon.width == 40.0),
            "fine-pointer tray icons stay 40px"
        );
        for icon in icon_buttons(&app_titlebar("文件", "#3498db", 1)) {
            assert_eq!(
                icon.width, 28.0,
                "{} left the title-bar exception",
                icon.label
            );
        }

        let shell = build_shell();
        assert_eq!(
            focusable_names(&shell),
            vec![cancel.clone(), confirm.clone()]
        );
        assert_eq!(initial_focus(&shell), vec![confirm.clone()]);
        assert!(
            !focusable_names(&shell).iter().any(|name| name == &propose),
            "propose stays outside the confirm trap"
        );

        state::set_text_signal(TERMINAL_SIGNAL, "echo hi");
        state::set_signal(PERM_SIGNAL, 1);
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "deny");
        let perm = build_shell();
        let deny = locale::t("perm-deny");
        let once = locale::t("perm-once");
        assert_eq!(focusable_names(&perm), vec![deny.clone(), once]);
        assert_eq!(initial_focus(&perm), vec![deny]);

        locale::set_locale("en");
        let english = build_shell();
        assert_eq!(
            focusable_names(&english),
            vec![locale::t("perm-deny"), locale::t("perm-once")]
        );
        locale::set_locale("zh-CN");

        state::set_signal(PHASE_SIGNAL, 0);
        let _ = state::take_focus_label();
        assert!(dismiss_top_layer());
        assert_eq!(state::get_signal(PERM_SIGNAL), 0);
        assert_eq!(
            state::take_focus_label().as_deref(),
            Some(locale::t("propose").as_str())
        );

        state::set_signal(PHASE_SIGNAL, 1);
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "confirm");
        let _ = state::take_focus_label();
        assert!(dismiss_top_layer());
        assert_eq!(state::get_signal(PHASE_SIGNAL), 0);
        assert_eq!(
            state::take_focus_label().as_deref(),
            Some(locale::t("propose").as_str())
        );

        state::note_pointer_coarse(true);
        state::note_shell_viewport(390.0, 760.0);
        state::set_signal(PHASE_SIGNAL, 1);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        let phone = build_confirm_dialog();
        assert!(button_extent(&phone, &locale::t("confirm")).1 >= 48.0);
        assert!(button_extent(&phone, &locale::t("cancel")).1 >= 48.0);
        let phone_go = button_extent(&build_intent_panel(), &locale::t("propose"));
        assert!(phone_go.0 >= 48.0 && phone_go.1 >= 48.0);

        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(UNLOCKED_SIGNAL, 0);
        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        locale::set_locale("zh-CN");
    }

    #[test]
    fn tier_badges_lead_with_words_and_l2_off_stays_on_the_chrome() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        locale::set_locale("zh-CN");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(UNLOCKED_SIGNAL, 1);
        state::set_signal(PHASE_SIGNAL, 1);
        state::set_signal(PERM_SIGNAL, 0);
        state::set_signal(TASKS_SIGNAL, 0);
        state::set_text_signal(SUMMARY_SIGNAL, "打开文件");
        state::set_text_signal(IMPACT_SIGNAL, "会打开文件窗口。");
        state::set_text_signal(TIER_SIGNAL, "L0");
        state::set_text_signal(CONFIRM_FOCUS_SIGNAL, "confirm");

        let mark = |tier: &str| {
            let card = layer_mark(tier).unwrap_or_else(|| panic!("missing {tier}"));
            let ComponentKind::Text { content } = &card.kind else {
                panic!("badge is not text");
            };
            (content.clone(), card.style.background)
        };
        let (l0, l0_bg) = mark("L0");
        assert_eq!(l0, "规则 L0");
        assert!(l0_bg.a > 200, "L0 badge background is missing");
        let (l1, l1_bg) = mark("L1");
        assert_eq!(l1, "同义 L1");
        assert_eq!((l1_bg.r, l1_bg.g, l1_bg.b), (0xf0, 0xd7, 0xb4));
        let (l2, l2_bg) = mark("L2");
        assert_eq!(l2, "规划 L2");
        assert_eq!((l2_bg.r, l2_bg.g, l2_bg.b), (0x2a, 0x31, 0x5c));

        let card = text_blob(&build_confirm_dialog());
        assert!(card.contains("规则 L0"), "{card}");
        state::set_text_signal(TIER_SIGNAL, "L1");
        let synonym = text_blob(&build_confirm_dialog());
        assert!(synonym.contains("同义 L1"), "{synonym}");

        let tasks = text_blob(&build_task_center());
        assert!(tasks.contains("没有模型在运行"), "{tasks}");
        assert!(!tasks.contains("后台线程"), "{tasks}");
        assert!(!tasks.contains("background thread"), "{tasks}");

        state::set_signal(OPEN_MASK_SIGNAL, app_bit(1));
        state::set_signal(APP_SIGNAL, 1);
        let bar = button_labels(&build_floating_taskbar());
        assert!(bar.iter().any(|label| label == "L2 关"), "{bar:?}");
        assert!(
            bar.iter()
                .any(|label| label.contains("文件") && label.contains("正在使用")),
            "{bar:?}"
        );
        assert!(
            button_labels(&build_phone_home())
                .iter()
                .any(|label| label == "L2 关")
        );
        assert!(
            button_labels(&build_tablet_rail())
                .iter()
                .any(|label| label == "L2 关")
        );
        let rail = button_labels(&build_tablet_rail());
        assert!(
            rail.iter()
                .any(|label| label.contains("文件") && label.contains("正在使用")),
            "{rail:?}"
        );

        let hidden = locale::t("toast-context-hidden");
        let shared = locale::t("toast-context-shared");
        assert!(!hidden.contains('×'), "{hidden}");
        assert!(hidden.contains("窗口"), "{hidden}");
        assert!(!shared.contains('×'), "{shared}");
        assert!(shared.contains("窗口"), "{shared}");
        for surface in [
            text_blob(&build_intent_panel()),
            text_blob(&build_floating_taskbar()),
            text_blob(&build_phone_home()),
            text_blob(&build_task_center()),
        ] {
            assert!(!surface.contains('×'), "{surface}");
        }

        locale::set_locale("en");
        assert_eq!(layer_label("L0"), "Rules L0");
        assert_eq!(layer_label("L1"), "Synonym L1");
        assert_eq!(layer_label("L2"), "Plan L2");
        assert!(!locale::t("planner-memory").contains("background thread"));
        assert!(!locale::t("toast-context-hidden").contains('×'));

        locale::set_locale("zh-CN");
        state::note_pointer_coarse(false);
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(PHASE_SIGNAL, 0);
        state::set_signal(OPEN_MASK_SIGNAL, 0);
        state::set_signal(APP_SIGNAL, 0);
        state::set_text_signal(TIER_SIGNAL, "");
    }

    #[test]
    fn infer_backend_toggle_uses_fluent_and_stays_out_of_process_by_default() {
        let _guard = POSTURE_LOCK.lock().unwrap();
        ensure_shell_signals();
        w3cos_ai_bridge::infer::clear_runtime_backend();
        state::note_shell_viewport(1200.0, 800.0);
        state::set_signal(PHASE_SIGNAL, 0);
        for tag in ["en", "zh-CN"] {
            locale::set_locale(tag);
            let page = Component::column(Style::default(), settings_l2_page());
            let blob = text_blob(&page);
            let current = locale::t("infer-backend-inferd");
            let toggle = locale::t("infer-backend-toggle");
            let detail = locale::t("infer-backend-detail");
            assert_ne!(current, "infer-backend-inferd", "{tag}");
            assert_ne!(toggle, "infer-backend-toggle", "{tag}");
            assert!(detail.contains("SHA-256"), "{tag} {detail}");
            assert!(blob.contains(&current), "{tag} {blob}");
            assert!(blob.contains(&toggle), "{tag} {blob}");
            assert!(blob.contains(&detail), "{tag} {blob}");
            assert_eq!(
                w3cos_ai_bridge::infer::active_backend(),
                w3cos_ai_bridge::infer::InferBackendKind::Inferd
            );
        }
        locale::set_locale("en");
        on_shell_intent(OP_INFER_BACKEND, "");
        #[cfg(not(feature = "in-process-infer"))]
        {
            assert_eq!(
                state::get_signal_text(TOAST_SIGNAL),
                locale::t("infer-backend-unavailable")
            );
            assert_eq!(
                w3cos_ai_bridge::infer::active_backend(),
                w3cos_ai_bridge::infer::InferBackendKind::Inferd
            );
        }
        #[cfg(feature = "in-process-infer")]
        {
            assert_eq!(
                state::get_signal_text(TOAST_SIGNAL),
                locale::t("toast-infer-in-process")
            );
            assert_eq!(
                w3cos_ai_bridge::infer::active_backend(),
                w3cos_ai_bridge::infer::InferBackendKind::InProcess
            );
            on_shell_intent(OP_INFER_BACKEND, "");
            assert_eq!(
                state::get_signal_text(TOAST_SIGNAL),
                locale::t("toast-infer-inferd")
            );
            assert_eq!(
                w3cos_ai_bridge::infer::active_backend(),
                w3cos_ai_bridge::infer::InferBackendKind::Inferd
            );
        }
        w3cos_ai_bridge::infer::clear_runtime_backend();
        locale::set_locale("zh-CN");
    }
}
