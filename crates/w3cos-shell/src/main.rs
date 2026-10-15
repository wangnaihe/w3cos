//! W3C OS Desktop Shell — the system-level GUI.
//!
//! Boot sequence: Linux kernel → init → S99w3cos → w3cos-shell (this binary)
//! Click an app icon → a floating window opens over the desktop.
//! Drag the title bar to move it; drag an edge or corner to resize.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

mod agent_host;
mod gesture;
mod handoff;
mod hpke;
mod ime_bar;
mod locale;
mod windows;

#[cfg(feature = "desktop")]
use crate::state;
use w3cos_ai_bridge::dom_access::ActionType;
use w3cos_ai_bridge::identity::LocalIdentityStub;
use w3cos_ai_bridge::intent::{
    ExecuteResult, IntentSession, IntentTier, ProposeResult, ShellApp, ShellContext,
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
    ensure_clock_timer();
    handoff::ensure_listener();
    agent_host::install_wake();
    w3cos_ai_bridge::server::set_intent_hooks(bridge_propose, bridge_confirm);
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

thread_local! {
    static SESSION: RefCell<Option<IntentSession>> = const { RefCell::new(None) };
    static SESSION_BOOTED: Cell<bool> = const { Cell::new(false) };
    static NOTICES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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
            _ => {}
        }
    }
}

fn on_shell_intent(code: u8, draft: &str) {
    if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
        eprintln!("[W3C OS][INTENT] code={code} draft={draft:?}");
    }
    ensure_shell_signals();
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
        OP_CONFIRM => apply_confirm(session, &ctx),
        OP_CANCEL => {
            if state::get_signal(PHASE_SIGNAL) == 3 || w3cos_ai_bridge::agent_loop::has_pending() {
                let update = agent_host::cancel();
                apply_host_update(session, update);
            } else {
                session.cancel();
                sync_session(session, 0, "");
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
            session.lock_identity();
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                &locale::t("toast-identity-locked"),
            );
        }
        OP_UNLOCK => {
            let passphrase = draft.trim();
            if session.unlock_with_passphrase(passphrase) {
                state::set_text_signal(LOCK_PASSPHRASE_SIGNAL, "");
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
            let notice = if session.begin_passkey_enrollment().is_some() {
                locale::t("toast-enroll-ok")
            } else {
                locale::t("toast-enroll-fail")
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
        OP_RUN_TERMINAL => open_process_permission(session),
        OP_PERM_ONCE => {
            state::set_signal(PERM_SIGNAL, 0);
            if session.identity_unlocked() {
                run_terminal_from_button(session);
            } else {
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    &locale::t("perm-locked"),
                );
            }
        }
        OP_PERM_DENY => deny_process_permission(session),
        OP_L2_TOGGLE => toggle_l2(session),
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
            state::set_text_signal(SUMMARY_SIGNAL, proposal.summary);
            state::set_text_signal(IMPACT_SIGNAL, proposal.impact);
            state::set_text_signal(TIER_SIGNAL, tier_name(proposal.tier));
            sync_session(session, 1, "");
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

fn apply_confirm(session: &mut IntentSession, ctx: &ShellContext) {
    if w3cos_ai_bridge::agent_loop::has_pending() {
        confirm_agent_step(session);
        return;
    }
    let Some(effect) = session.confirm(ctx, now_ms()) else {
        sync_session(session, 0, "");
        return;
    };
    apply_effect(&effect);
    sync_session(session, 2, &effect.receipt.outcome);
}

fn tier_name(tier: IntentTier) -> &'static str {
    match tier {
        IntentTier::L2 => "L2",
        IntentTier::L1 => "L1",
        IntentTier::L0 => "L0",
        IntentTier::Unspecified => "",
    }
}

fn on_agent_poll() {
    ensure_shell_signals();
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
    let outcome = if !granted {
        "Denied: w3cos-process is not granted. Nothing was changed.".to_string()
    } else {
        match shell_tools::run_allowlisted(&line) {
            Ok(output) => {
                state::set_text_signal(TERM_OUT_SIGNAL, &output);
                output
            }
            Err(err) => format!("Denied: {err}. Nothing was changed."),
        }
    };
    capability_gate::deny(shell_tools::CAP_PROCESS);
    let decision = if outcome.starts_with("Denied") {
        w3cos_ai_bridge::intent::Decision::Denied
    } else {
        w3cos_ai_bridge::intent::Decision::Allowed
    };
    session.record_external(&line, "l2:run", decision, true, &outcome, IntentTier::L2);
    sync_session(session, 2, &outcome);
}

fn apply_effect(effect: &ExecuteResult) {
    if let Some(app) = effect.navigate {
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
    state::set_text_signal(RECEIPT_SIGNAL, session.receipt_log_tail(48));
    if !notice.is_empty() {
        remember_notice(notice);
    }
    state::set_text_signal(STORAGE_SIGNAL, session.receipt_storage());
    state::set_text_signal(SUBJECT_SIGNAL, session.actor());
    state::set_text_signal(ENROLL_SIGNAL, session.enrollment_copy());
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
    let added = NOTICES.with(|slot| {
        let mut notices = slot.borrow_mut();
        if notices.last().is_some_and(|last| last == text) {
            return false;
        }
        notices.push(text.to_string());
        if notices.len() > 12 {
            notices.remove(0);
        }
        true
    });
    if added {
        state::set_signal(
            NOTIFY_UNREAD_SIGNAL,
            state::get_signal(NOTIFY_UNREAD_SIGNAL) + 1,
        );
    }
}

fn open_process_permission(session: &mut IntentSession) {
    state::set_signal(PERM_SIGNAL, 1);
    if !session.identity_unlocked() {
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            &locale::t("perm-locked"),
        );
    }
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

fn focus_app(id: i64) {
    ensure_shell_signals();
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
    let reserved = match posture() {
        Posture::Desktop => 96.0,
        Posture::WideTablet => 84.0,
        Posture::Tablet => 84.0,
        Posture::Phone => 72.0,
    };
    (width.max(320.0), (viewport_h - reserved).max(200.0))
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
    if state::get_signal(PERM_SIGNAL) != 0 {
        state::set_signal(PERM_SIGNAL, 0);
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
        state::get_signal(PERM_SIGNAL) != 0,
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
    if state::get_signal(PERM_SIGNAL) != 0 || state::get_signal(PHASE_SIGNAL) == 1 {
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
        if open & app_bit(id) != 0 {
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
        OP_SETTINGS_LOCAL => state::set_signal(SETTINGS_TAB_SIGNAL, 0),
        OP_SETTINGS_IDENTITY => state::set_signal(SETTINGS_TAB_SIGNAL, 1),
        OP_SETTINGS_BRIDGE => state::set_signal(SETTINGS_TAB_SIGNAL, 2),
        OP_SETTINGS_L2 => state::set_signal(SETTINGS_TAB_SIGNAL, 3),
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
        OP_FOCUS_AGENT => focus_app(4),
        OP_FOCUS_BROWSER => focus_app(5),
        OP_FOCUS_EDITOR => focus_app(6),
        OP_LOCALE_ZH => {
            locale::set_locale("zh-CN");
            state::mark_dirty();
        }
        OP_LOCALE_EN => {
            locale::set_locale("en");
            state::mark_dirty();
        }
        OP_NOTIFY_TOGGLE => {
            let next = i64::from(state::get_signal(NOTIFY_SIGNAL) == 0);
            state::set_signal(NOTIFY_SIGNAL, next);
            if next != 0 {
                state::set_signal(NOTIFY_UNREAD_SIGNAL, 0);
            }
        }
        OP_RECEIPT_PREV => {
            let page = state::get_signal(RECEIPT_PAGE_SIGNAL).saturating_sub(1);
            state::set_signal(RECEIPT_PAGE_SIGNAL, page);
        }
        OP_TRAY_L2 => {
            focus_app(3);
            state::set_signal(SETTINGS_TAB_SIGNAL, 3);
        }
        OP_RECEIPT_NEXT => {
            let pages = receipt_page_count(visible_receipts().len());
            let page = (state::get_signal(RECEIPT_PAGE_SIGNAL) + 1).min(pages - 1);
            state::set_signal(RECEIPT_PAGE_SIGNAL, page);
        }
        OP_CLOCK => {
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
            state::set_signal(SETTINGS_TAB_SIGNAL, 4);
        }
        OP_PAIR_NO => {
            handoff::codes_differ();
            state::mark_dirty();
        }
        OP_PAIR_YES => {
            let _ = handoff::codes_match();
            state::mark_dirty();
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
        OP_HANDOFF_LATER => {
            handoff::hide_banner();
            state::mark_dirty();
        }
        OP_HANDOFF_GO => {
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
        }
        OP_IME_MODE => {
            ime_bar::toggle_mode();
            apply_ime_field();
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

fn session_l2_on() -> bool {
    SESSION.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|session| session.l2_enabled())
    })
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
        Some("settings") => focus_app(3),
        Some("settings-l2") => {
            focus_app(3);
            state::set_signal(SETTINGS_TAB_SIGNAL, 3);
        }
        Some("settings-identity") => {
            focus_app(3);
            state::set_signal(SETTINGS_TAB_SIGNAL, 1);
        }
        Some("settings-bridge") => {
            focus_app(3);
            state::set_signal(SETTINGS_TAB_SIGNAL, 2);
        }
        Some("files") => focus_app(1),
        Some("files-docs") => {
            focus_app(1);
            state::set_signal(FILES_FOLDER_SIGNAL, 1);
        }
        Some("terminal") => focus_app(2),
        Some("launcher") => state::set_signal(LAUNCHER_SIGNAL, 1),
        Some("receipts") => {
            state::set_signal(RECEIPTS_OPEN_SIGNAL, 1);
            state::set_text_signal(
                RECEIPT_SIGNAL,
                "#4 allowed open-files tier=L0 actor=local confirmed=true ts=1760518800000 — 已切换到文件 @@ 打开文件\n\
                 #3 cancelled night-mode tier=L1 actor=local confirmed=false ts=1760515200000 — 你取消了，没有执行 @@ 夜间模式\n\
                 #2 denied echo tier=L0 actor=local confirmed=true ts=1760511600000 — 当时还锁着 @@ 在终端运行 echo\n\
                 #1 unrecognized summarize tier=L0 actor=local confirmed=false ts=1760508000000 — 没有对应命令 @@ 把这段总结一下",
            );
        }
        Some("confirm") => {
            focus_app(1);
            state::set_signal(PHASE_SIGNAL, 1);
            state::set_text_signal(TIER_SIGNAL, "L0");
            state::set_text_signal(SUMMARY_SIGNAL, "打开文件");
            state::set_text_signal(
                IMPACT_SIGNAL,
                "切换到文件窗口。不读取、不修改磁盘上的内容。",
            );
        }
        Some("permission") => {
            focus_app(2);
            state::set_text_signal(TERMINAL_SIGNAL, "echo");
            state::set_signal(PERM_SIGNAL, 1);
        }
        Some("lock") => state::set_signal(UNLOCKED_SIGNAL, 0),
        Some("switcher") => {
            focus_app(1);
            state::set_signal(SWITCHER_SIGNAL, 1);
        }
        Some("notify") => state::set_signal(NOTIFY_SIGNAL, 1),
        Some("devices") => {
            focus_app(3);
            state::set_signal(SETTINGS_TAB_SIGNAL, 4);
            handoff::demo_pending();
        }
        Some("devices-sync") => {
            focus_app(3);
            state::set_signal(SETTINGS_TAB_SIGNAL, 4);
            handoff::demo_pending();
            let _ = handoff::codes_match();
        }
        Some("handoff") => {
            focus_app(1);
            handoff::demo_banner();
        }
        Some("handoff-phone") => {
            focus_app(0);
            handoff::demo_incoming();
        }
        Some("windows") => {
            state::set_signal(OPEN_MASK_SIGNAL, app_bit(1) | app_bit(2) | app_bit(5));
            state::set_signal(MINIMIZED_MASK_SIGNAL, app_bit(5));
            focus_app(1);
            push_toast(&locale::t("toast-browser-min"));
        }
        Some("tasks") => state::set_signal(TASKS_SIGNAL, 1),
        _ => {}
    }
}

fn build_shell() -> Component {
    ensure_shell_signals();
    if let Some(session) = windows::load_once() {
        state::set_signal(OPEN_MASK_SIGNAL, session.open);
        state::set_signal(MINIMIZED_MASK_SIGNAL, session.minimized);
        state::set_signal(APP_SIGNAL, session.focused.clamp(0, 6));
    }
    boot_session_once();
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
    if handoff::show_banner() && matches!(posture(), Posture::Desktop | Posture::WideTablet) {
        column.push(chrome_layer(build_handoff_banner()));
    }
    if handoff::incoming().is_some() {
        column.push(chrome_layer(build_handoff_incoming()));
    }
    if state::get_signal(TASKS_SIGNAL) != 0 && posture() == Posture::Desktop {
        column.push(dock_right(build_task_center()));
    }
    if state::get_signal(PHASE_SIGNAL) == 1 {
        column.push(modal_layer(build_confirm_dialog()));
    }
    if state::get_signal(PERM_SIGNAL) != 0 {
        column.push(modal_layer(build_permission_sheet()));
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
    if let Some(bar) = ime_bar::candidate_layer() {
        column.push(bar);
    }
    if !unlocked {
        column.push(place_overlay(build_lock_screen()));
    }
    if let Some(keyboard) = ime_bar::phone_keyboard() {
        column.push(keyboard);
    }

    Component::column(
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
    )
}

fn modal_layer(card: Component) -> Component {
    place_modal(card, false)
}

fn place_modal(mut card: Component, fill: bool) -> Component {
    let (width, height) = state::shell_viewport();
    let scrim_h = if fill || posture() == Posture::Phone {
        height
    } else {
        (height - 68.0).max(0.0)
    };
    let scrim = Component::button_with_click(
        "",
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
        invoke(OP_SCRIM),
    );
    card.style.position = Position::Absolute;
    card.style.z_index = CHROME_Z + 3;
    if fill {
        card.style.left = Dimension::Px(0.0);
        card.style.top = Dimension::Px(0.0);
        card.style.width = Dimension::Px(width);
        card.style.height = Dimension::Px(height);
    } else if posture() == Posture::Phone {
        let sheet_h = 280.0_f32.min(height - 24.0);
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
    let phone = posture() == Posture::Phone;
    let panel_w = if phone {
        340.0_f32.min(width)
    } else {
        400.0_f32.min((width - 48.0).max(280.0))
    };
    let top = if phone { 0.0 } else { 24.0 };
    let bottom = (intent_block_top(height) - 12.0).max(top);
    panel.style.position = Position::Absolute;
    panel.style.left = Dimension::Px(if phone {
        (width - panel_w).max(0.0)
    } else {
        (width - panel_w - 24.0).max(8.0)
    });
    panel.style.top = Dimension::Px(top);
    panel.style.width = Dimension::Px(panel_w);
    panel.style.height = Dimension::Px(bottom - top);
    panel.style.z_index = CHROME_Z + 1;
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
        color: "#9b59b6",
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

fn touch_min() -> f32 {
    if state::pointer_is_coarse() {
        48.0
    } else {
        36.0
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
        "bell" => {
            r#"<path d="M6 16.5h12l-1.2-2V10.2a4.8 4.8 0 0 0-9.6 0v4.3L6 16.5Z"/><path d="M10 16.5a2 2 0 0 0 4 0"/>"#
        }
        "devices" => {
            r#"<rect x="3" y="6" width="12" height="8" rx="1.4"/><path d="M7 17.5h6M10 14v3.5"/><rect x="16" y="8" width="5" height="9" rx="1.2"/>"#
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

fn set_receipt_filter(kind: i64) {
    state::set_signal(RECEIPT_FILTER_SIGNAL, kind);
    state::set_signal(RECEIPT_PAGE_SIGNAL, 0);
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

fn l2_badge() -> String {
    if session_l2_on() {
        locale::t("l2-badge-on")
    } else {
        locale::t("l2-badge-off")
    }
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
    detail: String,
    recorded_at: String,
    utterance: String,
}

fn parse_receipt(line: &str) -> ReceiptParts {
    let (head, rest) = line.split_once(" — ").unwrap_or((line, ""));
    let (detail, utterance) = rest.split_once(" @@ ").unwrap_or((rest, ""));
    let mut tokens = head.split_whitespace();
    let id = tokens.next().unwrap_or("#").to_string();
    let decision = tokens.next().unwrap_or("").to_string();
    let command = tokens.next().unwrap_or("").to_string();
    let mut tier = String::new();
    let mut recorded_at = String::new();
    for token in tokens {
        if let Some(value) = token.strip_prefix("tier=") {
            tier = value.to_string();
        } else if let Some(value) = token.strip_prefix("ts=") {
            recorded_at = value.to_string();
        }
    }
    ReceiptParts {
        id,
        decision,
        command,
        tier,
        detail: detail.to_string(),
        recorded_at,
        utterance: utterance.to_string(),
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
    let go = if state::pointer_is_coarse() {
        44.0
    } else {
        36.0
    };
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
    if state::get_signal(PHASE_SIGNAL) == 3 {
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

fn taskbar_app(id: i64, current: i64) -> Component {
    let focused = current == id;
    let glyph = RESIDENT_APPS
        .iter()
        .find(|app| app.id == id)
        .map(|app| app.glyph)
        .unwrap_or("grid");
    let (fill, _) = squircle_tone(glyph);
    dock_icon(glyph, app_label(id), focus_code(id), fill, focused)
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
            "lock",
            if unlocked {
                locale::t("lock")
            } else {
                locale::t("unlock")
            },
            if unlocked { OP_LOCK } else { OP_UNLOCK },
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
        dock_icon(
            "bell",
            locale::t("tray-notify"),
            OP_NOTIFY_TOGGLE,
            Color::TRANSPARENT,
            false,
        ),
        Component::button_with_click(
            l2_badge(),
            Style {
                background: Color::TRANSPARENT,
                color: faint_token(),
                font_size: 12.0,
                padding: Edges::xy(8.0, 0.0),
                min_height: Dimension::Px(if state::pointer_is_coarse() {
                    48.0
                } else {
                    32.0
                }),
                border_radius: 8.0,
                ..Style::default()
            },
            invoke(OP_TRAY_L2),
        ),
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
    tray.push(Component::text(
        live_clock(),
        Style {
            font_size: 13.0,
            font_weight: 600,
            color: ink_token(),
            padding: Edges::xy(10.0, 0.0),
            ..Style::default()
        },
    ));
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
    let mut status_right = vec![Component::svg_document(
        glyph_svg("lock", "#f4f1ea"),
        24,
        24,
        Style {
            width: Dimension::Px(14.0),
            height: Dimension::Px(14.0),
            ..Style::default()
        },
    )];
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
                Component::text(
                    live_clock(),
                    Style {
                        font_size: 14.0,
                        font_weight: 650,
                        color: ink_token(),
                        ..Style::default()
                    },
                ),
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
        "#5b4fe0",
        48.0,
    )];
    for app in RESIDENT_APPS {
        let mut button = icon_button(
            app.glyph,
            String::new(),
            focus_code(app.id),
            app.color,
            48.0,
        );
        if current == app.id {
            button.style.border_width = 2.0;
            button.style.border_color = Color::from_hex("#f4f7fb");
        }
        icons.push(button);
    }
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

fn build_confirm_dialog() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let tier = state::get_signal_text(TIER_SIGNAL);
    let mut rows = Vec::new();
    if tier == "L0" || tier == "L1" || tier == "L2" {
        let (label, bg, fg) = if tier == "L2" {
            (
                locale::t("tier-l2"),
                Color::from_hex("#2a315c"),
                Color::from_hex("#b7c0ff"),
            )
        } else if tier == "L1" {
            (
                locale::t("tier-l1"),
                Color::from_hex("#f0d7b4"),
                Color::from_hex("#2a1c0c"),
            )
        } else {
            (
                locale::t("tier-l0"),
                Color::rgba(244, 241, 234, 20),
                ink_token(),
            )
        };
        rows.push(Component::text(
            label,
            Style {
                font_size: 12.0,
                font_weight: 650,
                color: fg,
                background: bg,
                border_radius: 999.0,
                padding: Edges::xy(8.0, 2.0),
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
        ));
    }
    rows.push(Component::text(
        state::get_signal_text(SUMMARY_SIGNAL),
        Style {
            font_size: 18.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    ));
    rows.push(Component::text(
        state::get_signal_text(IMPACT_SIGNAL),
        Style {
            font_size: 13.0,
            color: muted_ink(),
            ..Style::default()
        },
    ));
    let accept = if unlocked {
        sheet_button(locale::t("confirm"), OP_CONFIRM, SheetKind::Primary)
    } else {
        sheet_button(locale::t("lock-first"), OP_UNLOCK, SheetKind::Primary)
    };
    rows.push(Component::row(
        Style {
            gap: 8.0,
            justify_content: JustifyContent::FlexEnd,
            ..Style::default()
        },
        vec![
            sheet_button(locale::t("cancel"), OP_CANCEL, SheetKind::Ghost),
            accept,
        ],
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

fn build_permission_sheet() -> Component {
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let command = process_command();
    let mut rows = vec![Component::text(
        locale::t_args("perm-sheet-title", &[("command", &command)]),
        Style {
            font_size: 18.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    )];
    if unlocked {
        rows.push(Component::text(
            locale::t("perm-sheet-detail"),
            Style {
                font_size: 13.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
        rows.push(Component::row(
            Style {
                gap: 12.0,
                padding: Edges::all(12.0),
                border_radius: 12.0,
                border_width: 1.0,
                border_color: Color::rgba(244, 241, 234, 31),
                background: Color::rgba(244, 241, 234, 10),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                ..Style::default()
            },
            vec![
                Component::column(
                    Style {
                        gap: 4.0,
                        flex_grow: 1.0,
                        ..Style::default()
                    },
                    vec![
                        Component::text(
                            locale::t("perm-cap-title"),
                            Style {
                                font_size: 13.0,
                                font_weight: 650,
                                color: ink_token(),
                                ..Style::default()
                            },
                        ),
                        Component::text(
                            locale::t("perm-sheet-scope"),
                            Style {
                                font_size: 12.0,
                                color: faint_token(),
                                ..Style::default()
                            },
                        ),
                    ],
                ),
                Component::text(
                    locale::t("perm-cap-id"),
                    Style {
                        font_size: 12.0,
                        color: faint_token(),
                        ..Style::default()
                    },
                ),
            ],
        ));
        rows.push(Component::row(
            Style {
                gap: 8.0,
                justify_content: JustifyContent::FlexEnd,
                ..Style::default()
            },
            vec![
                sheet_button(locale::t("perm-deny"), OP_PERM_DENY, SheetKind::Danger),
                sheet_button(locale::t("perm-once"), OP_PERM_ONCE, SheetKind::Primary),
            ],
        ));
    } else {
        rows.push(Component::text(
            locale::t("perm-locked"),
            Style {
                font_size: 14.0,
                font_weight: 700,
                color: Color::from_hex("#ffe08a"),
                ..Style::default()
            },
        ));
        rows.push(Component::row(
            Style {
                gap: 8.0,
                ..Style::default()
            },
            vec![
                sheet_button(locale::t("perm-deny"), OP_PERM_DENY, SheetKind::Danger),
                {
                    let mut unlock =
                        sheet_button(locale::t("lock-action"), OP_UNLOCK, SheetKind::Primary);
                    unlock.on_click = EventAction::Invoke {
                        code: OP_UNLOCK,
                        input_placeholder: locale::t("lock-passphrase"),
                    };
                    unlock
                },
            ],
        ));
    }
    dialog_shell(rows)
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

fn build_notify_panel() -> Component {
    let (width, height) = state::shell_viewport();
    let panel_w = 320.0_f32.min(width - 24.0);
    let notices = NOTICES.with(|slot| slot.borrow().clone());
    let mut body = vec![
        Component::text(
            locale::t("notify-title"),
            Style {
                font_size: 16.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ),
        chrome_button(locale::t("close"), OP_NOTIFY_TOGGLE, "#3d5570"),
    ];
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
            body.push(Component::text(
                notice.clone(),
                Style {
                    font_size: 13.0,
                    color: ink(),
                    ..Style::default()
                },
            ));
        }
    }
    let bottom = intent_block_top(height) - 12.0;
    let top = (bottom - 240.0).max(16.0);
    Component::column(
        Style {
            position: Position::Absolute,
            left: Dimension::Px((width - panel_w - 16.0).max(8.0)),
            top: Dimension::Px(top),
            width: Dimension::Px(panel_w),
            max_height: Dimension::Px((bottom - top).max(0.0)),
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
        4 => build_ai_agent_app(),
        5 => build_browser_app(),
        _ => build_editor_app(),
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
    let bg = if active { "#3d5570" } else { "#243447" };
    let color = if active { "#f4f7fb" } else { "#d5dee8" };
    Component::button_with_click(
        name,
        Style {
            font_size: 13.0,
            color: Color::from_hex(color),
            background: Color::from_hex(bg),
            border_radius: 10.0,
            padding: Edges::xy(12.0, 12.0),
            min_height: Dimension::Px(48.0),
            ..Style::default()
        },
        invoke(code),
    )
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
                    color: Color::from_hex("#6c5ce7"),
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
                                "user@w3cos:~$",
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

fn build_settings_app() -> Component {
    let tab = state::get_signal(SETTINGS_TAB_SIGNAL);
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#24384a"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(locale::t("app-settings"), "#5b4fe0", 3),
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
                            background: Color::from_hex("#1b2c3e"),
                            padding: Edges::all(12.0),
                            gap: 4.0,
                            ..Style::default()
                        },
                        vec![
                            nav_button(&locale::t("settings-local"), tab == 0, OP_SETTINGS_LOCAL),
                            nav_button(
                                &locale::t("settings-identity"),
                                tab == 1,
                                OP_SETTINGS_IDENTITY,
                            ),
                            nav_button(
                                &locale::t("settings-permissions"),
                                tab == 2,
                                OP_SETTINGS_BRIDGE,
                            ),
                            nav_button(&locale::t("settings-l2"), tab == 3, OP_SETTINGS_L2),
                            nav_button(
                                &locale::t("settings-devices"),
                                tab == 4,
                                OP_SETTINGS_DEVICES,
                            ),
                        ],
                    ),
                    Component::column(
                        Style {
                            flex_grow: 1.0,
                            padding: Edges::all(20.0),
                            gap: 12.0,
                            ..Style::default()
                        },
                        settings_page(tab),
                    ),
                ],
            ),
        ],
    )
}

fn settings_page(tab: i64) -> Vec<Component> {
    match tab {
        1 => settings_identity_page(),
        2 => settings_permissions_page(),
        3 => settings_l2_page(),
        4 => settings_devices_page(),
        _ => settings_local_page(),
    }
}

fn settings_devices_page() -> Vec<Component> {
    let mut page = settings_heading(
        &locale::t("settings-devices-title"),
        &locale::t("settings-devices-detail"),
    );
    let fingerprint = truncate_middle(&handoff::local_fingerprint(), 28);
    page.push(Component::text(
        format!("{} · {fingerprint}", handoff::local_name()),
        Style {
            font_size: 14.0,
            color: ink(),
            ..Style::default()
        },
    ));
    if let Some(peer) = handoff::pending_peer() {
        let (grouped, spaced) = handoff::short_code(
            &handoff::local_fingerprint(),
            &peer.fingerprint,
            &peer.nonce,
        );
        page.push(Component::text(
            format!("{} · {}", peer.name, locale::t("pair-pending")),
            Style {
                font_size: 14.0,
                color: Color::from_hex("#f0d7b4"),
                ..Style::default()
            },
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
    } else if handoff::choosing_sync() {
        page.push(Component::text(
            locale::t("pair-trusted"),
            Style {
                font_size: 14.0,
                color: ink(),
                ..Style::default()
            },
        ));
        page.push(chrome_button(
            locale::t("sync-receipts"),
            OP_SYNC_RECEIPTS,
            "#0c6d4c",
        ));
        page.push(chrome_button(
            locale::t("sync-drafts"),
            OP_SYNC_DRAFTS,
            "#0c6d4c",
        ));
        page.push(chrome_button(
            locale::t("sync-clipboard"),
            OP_SYNC_CLIPBOARD,
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
                locale::t("pair-trusted")
            } else {
                locale::t("pair-pending")
            };
            page.push(Component::text(
                format!("{} · {state}", peer.name),
                Style {
                    font_size: 14.0,
                    color: ink(),
                    ..Style::default()
                },
            ));
        }
    }
    if handoff::sending() {
        page.push(Component::text(
            locale::t("handoff-sending"),
            Style {
                font_size: 14.0,
                color: ink(),
                ..Style::default()
            },
        ));
    }
    let notice = handoff::notice();
    if !notice.is_empty() {
        page.push(Component::text(
            locale::t(&notice),
            Style {
                font_size: 13.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    }
    page
}

fn build_handoff_banner() -> Component {
    let mut card = Component::column(
        Style {
            width: Dimension::Px(420.0),
            padding: Edges::all(16.0),
            gap: 8.0,
            background: Color::from_hex("#1c2631"),
            border_radius: 16.0,
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t("handoff-title"),
                Style {
                    font_size: 16.0,
                    font_weight: 700,
                    color: ink(),
                    ..Style::default()
                },
            ),
            Component::text(
                locale::t("handoff-detail"),
                Style {
                    font_size: 13.0,
                    color: muted_ink(),
                    ..Style::default()
                },
            ),
            chrome_button(locale::t("handoff-later"), OP_HANDOFF_LATER, "#3d5570"),
            chrome_button(locale::t("handoff-go"), OP_HANDOFF_GO, "#0c6d4c"),
        ],
    );
    let (width, _) = state::shell_viewport();
    card.style.position = Position::Absolute;
    card.style.left = Dimension::Px(((width - 420.0) / 2.0).max(16.0));
    card.style.top = Dimension::Px(24.0);
    card.style.z_index = CHROME_Z + 1;
    card
}

fn build_handoff_incoming() -> Component {
    let (from, path, draft) = handoff::incoming().unwrap_or_default();
    let (width, height) = state::shell_viewport();
    let mut card = Component::column(
        Style {
            padding: Edges::all(20.0),
            gap: 12.0,
            background: Color::from_hex("#1c2631"),
            border_radius: 22.0,
            ..Style::default()
        },
        vec![
            Component::text(
                locale::t_args("handoff-from", &[("name", &from)]),
                Style {
                    font_size: 20.0,
                    font_weight: 700,
                    color: ink(),
                    ..Style::default()
                },
            ),
            Component::text(
                path,
                Style {
                    font_size: 14.0,
                    color: ink(),
                    ..Style::default()
                },
            ),
            Component::text(
                draft,
                Style {
                    font_size: 14.0,
                    color: muted_ink(),
                    ..Style::default()
                },
            ),
            Component::text(
                locale::t("handoff-l2-still-off"),
                Style {
                    font_size: 13.0,
                    color: muted_ink(),
                    ..Style::default()
                },
            ),
            chrome_button(locale::t("handoff-open"), OP_HANDOFF_OPEN, "#0c6d4c"),
            chrome_button(locale::t("handoff-later"), OP_HANDOFF_HIDE, "#3d5570"),
        ],
    );
    card.style.position = Position::Absolute;
    card.style.left = Dimension::Px(16.0);
    card.style.top = Dimension::Px(24.0);
    card.style.width = Dimension::Px((width - 32.0).max(200.0));
    card.style.max_height = Dimension::Px((height - 120.0).max(240.0));
    card.style.z_index = CHROME_Z + 1;
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

fn settings_local_page() -> Vec<Component> {
    let night = if state::get_signal(NIGHT_SIGNAL) != 0 {
        locale::t("on")
    } else {
        locale::t("off")
    };
    let mut rows = settings_heading(
        &locale::t("settings-local-title"),
        &locale::t("settings-local-detail"),
    );
    rows.push(settings_row(&locale::t("settings-night"), &night));
    rows.push(chrome_button(
        locale::t("settings-night-toggle"),
        OP_CHIP_NIGHT,
        "#3d5570",
    ));
    rows.push(settings_row(
        &locale::t("settings-language"),
        &locale::current(),
    ));
    rows.push(chrome_button(locale::t("lang-zh"), OP_LOCALE_ZH, "#3d5570"));
    rows.push(chrome_button(locale::t("lang-en"), OP_LOCALE_EN, "#3d5570"));
    rows
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
    let enroll_owned = state::get_signal_text(ENROLL_SIGNAL);
    let enroll_label = if enroll_owned.is_empty() {
        locale::t("enroll-fallback")
    } else {
        enroll_owned
    };
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut rows = settings_heading(
        &locale::t("settings-identity-title"),
        &locale::t("settings-identity-detail"),
    );
    rows.push(settings_row(&locale::t("settings-subject"), &subject_label));
    rows.push(Component::text(
        enroll_label,
        Style {
            font_size: 13.0,
            color: Color::from_hex("#d5dee8"),
            ..Style::default()
        },
    ));
    rows.push(chrome_button(
        locale::t("enroll-challenge"),
        OP_ENROLL,
        "#3d5570",
    ));
    rows.push(settings_row(
        &locale::t("settings-identity-title"),
        &locale::t("settings-not-passkey"),
    ));
    let status = if unlocked {
        locale::t("unlocked")
    } else {
        locale::t("locked")
    };
    rows.push(settings_row(&locale::t("settings-status"), &status));
    rows.push(chrome_button(
        if unlocked {
            locale::t("lock")
        } else {
            locale::t("unlock")
        },
        if unlocked { OP_LOCK } else { OP_UNLOCK },
        if unlocked { "#3d5570" } else { "#9a3412" },
    ));
    rows.push(settings_row(&locale::t("settings-storage"), &storage_label));
    rows.push(chrome_button(
        locale::t("open-ledger"),
        OP_TOGGLE_RECEIPTS,
        "#3d5570",
    ));
    rows
}

fn settings_permissions_page() -> Vec<Component> {
    let bridge = std::env::var("W3COS_AI_PERMISSIONS").unwrap_or_default();
    let bridge = match bridge.trim() {
        "" | "observer" => locale::t("perm-observer"),
        "interactive" => locale::t("perm-interactive"),
        "system" => locale::t("perm-system"),
        _ => locale::t("perm-unknown"),
    };
    let mut rows = settings_heading(&locale::t("perm-title"), &locale::t("perm-detail"));
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
        Component::text(
            locale::t("tasks-title"),
            Style {
                font_size: 16.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
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
                        locale::t("state-stub-unlocked")
                    } else {
                        locale::t("state-locked")
                    };
                    locale::t_args(
                        "identity-line",
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
    body.push(chrome_button(
        if unlocked {
            locale::t("lock")
        } else {
            locale::t("unlock-to-run")
        },
        if unlocked { OP_LOCK } else { OP_UNLOCK },
        if unlocked { "#3d5570" } else { "#9a3412" },
    ));
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
    match std::env::var("W3COS_L2").ok().as_deref().map(str::trim) {
        Some("script") => locale::t("planner-script"),
        Some("1") | Some("true") => {
            let model = std::env::var("W3COS_L2_MODEL").unwrap_or_else(|_| "local".into());
            locale::t_args("planner-model", &[("model", &model)])
        }
        _ => {
            if state::get_signal_text(STORAGE_SIGNAL) == "encrypted-file" {
                locale::t("planner-encrypted")
            } else {
                locale::t("planner-memory")
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SheetKind {
    Ghost,
    Primary,
    Danger,
}

fn action_button(label: impl Into<String>, code: u8, kind: SheetKind) -> Component {
    let (background, color, border) = match kind {
        SheetKind::Ghost => (Color::TRANSPARENT, ink_token(), Some(line_strong())),
        SheetKind::Primary => (Color::from_hex("#0c6d4c"), Color::from_hex("#e9fff5"), None),
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
// AI Agent window — the task center stays beside whatever page is open.
// ==========================================================================

fn build_ai_agent_app() -> Component {
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#24506e"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar(locale::t("app-agent"), "#e07a5f", 4),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    padding: Edges::all(24.0),
                    gap: 12.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        locale::t("agent-body"),
                        Style {
                            font_size: 16.0,
                            font_weight: 700,
                            color: ink(),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        state::get_signal_text(RECEIPT_SIGNAL),
                        Style {
                            font_size: 13.0,
                            color: ink(),
                            ..Style::default()
                        },
                    ),
                ],
            ),
        ],
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
                            color: Color::from_hex("#6c5ce7"),
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
                                            background: Color::from_hex("#6c5ce7"),
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
                    background: Color::from_hex("#6c5ce7"),
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
    let border_color = if active { "#6c5ce7" } else { "#14142a" };
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
                    color: Color::from_hex("#6c5ce7"),
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
    let (max_glyph, max_label) = if windows::is_maximized(app_id) {
        ("restore", locale::t("win-restore"))
    } else {
        ("maximize", locale::t("win-maximize"))
    };
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
                    gap: 6.0,
                    ..Style::default()
                },
                vec![
                    window_button(
                        "minus",
                        locale::t("win-minimize"),
                        OP_MINIMIZE,
                        "#c5d0d8",
                        app_id,
                    ),
                    window_button(max_glyph, max_label, OP_MAXIMIZE, "#c5d0d8", app_id),
                    window_button("close", locale::t("win-close"), OP_CLOSE, "#e94560", app_id),
                ],
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
        color: "#6c5ce7",
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
        color: "#9b59b6",
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
                        background: Color::from_hex("#5b4fe0"),
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
    let (pill_bg, pill_fg) = match parts.decision.as_str() {
        "allowed" => (Color::rgba(20, 150, 106, 46), Color::from_hex("#c9ffe4")),
        "denied" => (Color::rgba(224, 122, 120, 41), Color::from_hex("#ffd0cf")),
        "cancelled" => (Color::rgba(226, 164, 92, 41), Color::from_hex("#ffe1b5")),
        _ => (Color::rgba(244, 241, 234, 20), muted_token()),
    };
    let clock = format_receipt_clock(&parts.recorded_at);
    let said = if parts.utterance.is_empty() {
        parts.detail.clone()
    } else {
        parts.utterance.clone()
    };
    let mut subtitle_bits = Vec::new();
    if !clock.is_empty() {
        subtitle_bits.push(clock);
    }
    if !parts.tier.is_empty() {
        subtitle_bits.push(parts.tier.clone());
    }
    if !said.is_empty() {
        subtitle_bits.push(said);
    }
    let subtitle = subtitle_bits.join(" · ");
    Component::row(
        Style {
            gap: 8.0,
            align_items: AlignItems::Center,
            padding: Edges::xy(8.0, 10.0),
            border_width: 0.0,
            border_color: Color::rgba(244, 241, 234, 31),
            ..Style::default()
        },
        vec![
            Component::text(
                parts.id,
                Style {
                    width: Dimension::Px(52.0),
                    font_size: 12.0,
                    color: faint_token(),
                    flex_shrink: 0.0,
                    ..Style::default()
                },
            ),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    min_width: Dimension::Px(0.0),
                    gap: 2.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        parts.command,
                        Style {
                            font_size: 13.0,
                            font_weight: 650,
                            color: ink_token(),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        subtitle,
                        Style {
                            font_size: 12.0,
                            color: faint_token(),
                            ..Style::default()
                        },
                    ),
                ],
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
    )
}

fn open_app_ids() -> Vec<i64> {
    let mask = state::get_signal(OPEN_MASK_SIGNAL);
    (1..=6).filter(|id| mask & app_bit(*id) != 0).collect()
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
    let storage_owned = state::get_signal_text(STORAGE_SIGNAL);
    let storage_label = if storage_owned.is_empty() {
        "memory".to_string()
    } else {
        storage_owned
    };
    let subject_label = truncate_middle(&current_subject(), 22);
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
            format!("{subject_label} · {storage_label}"),
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
        Component::row(
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
        ),
    ];
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
        assert!(parts.recorded_at.is_empty());
        assert!(parts.utterance.is_empty());
        let stamped = parse_receipt(
            "#4 allowed open-files tier=L0 actor=local confirmed=true ts=1760518800000 — 已切换到文件 @@ 打开文件",
        );
        assert_eq!(stamped.recorded_at, "1760518800000");
        assert_eq!(stamped.detail, "已切换到文件");
        assert_eq!(stamped.utterance, "打开文件");
        assert_eq!(format_receipt_clock("0"), "");
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
        locale::set_locale("zh-CN");
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
        locale::set_locale("en");
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
        for (width, height) in [(1200.0, 800.0), (390.0, 760.0)] {
            state::note_shell_viewport(width, height);
            let panel = place_receipts(Component::column(Style::default(), vec![]));
            let bottom = px(panel.style.top) + px(panel.style.height);
            let intent_top = px(floating_intent().style.top);
            assert!(
                bottom <= intent_top - 8.0,
                "{width}x{height}: drawer ends at {bottom}, intent starts at {intent_top}"
            );
        }
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
        let app = build_terminal_app();
        let mut texts = Vec::new();
        walk(&app, &mut |node| {
            if let ComponentKind::Text { content } = &node.kind {
                texts.push(content.clone());
            }
        });
        let grants = texts
            .iter()
            .filter(|text| text.contains("echo") || text.contains("w3cos-process"))
            .count();
        assert_eq!(grants, 1, "{texts:?}");
        assert!(texts.iter().any(|text| text.contains("终端已就绪")));
        locale::set_locale("en");
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
}
