//! W3C OS Desktop Shell — the system-level GUI.
//!
//! Boot sequence: Linux kernel → init → S99w3cos → w3cos-shell (this binary)
//! Click an app icon → a floating window opens over the desktop.
//! Drag the title bar to move it; drag an edge or corner to resize.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

mod agent_host;
mod windows;

#[cfg(feature = "desktop")]
use crate::state;
use w3cos_ai_bridge::dom_access::ActionType;
use w3cos_ai_bridge::identity::LocalIdentityStub;
use w3cos_ai_bridge::intent::{
    ExecuteResult, INTENT_INPUT_PLACEHOLDER, IntentSession, IntentTier, ProposeResult, ShellApp,
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

const LAUNCHER_PLACEHOLDER: &str = "搜索应用 Search apps";

fn main() {
    shell_main();
}

pub(crate) fn shell_main() {
    if shared_launch::launch_from_args() {
        return;
    }
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
    state::set_agent_poll_hook(on_agent_poll);
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
        let _ = state::create_signal(1);
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
        sync_session(session, 0, "");
    });
}

fn on_shell_intent(code: u8, draft: &str) {
    if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
        eprintln!("[W3C OS][INTENT] code={code} draft={draft:?}");
    }
    ensure_shell_signals();
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
                "上下文已隐藏。Context hidden.",
            );
        }
        OP_RESTORE => {
            session.restore_context();
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                "上下文已恢复。Context shared.",
            );
        }
        OP_LOCK => {
            session.lock_identity();
            sync_session(
                session,
                state::get_signal(PHASE_SIGNAL),
                "身份已锁定。Identity locked.",
            );
        }
        OP_UNLOCK => {
            if session.unlock_identity_stub("stub-passkey") {
                sync_session(
                    session,
                    state::get_signal(PHASE_SIGNAL),
                    "已解锁（不是 passkey）。Unlocked stub.",
                );
            }
        }
        OP_ENROLL => {
            let notice = if session.begin_passkey_enrollment().is_some() {
                "已生成注册挑战。还没有验证断言。Challenge ready."
            } else {
                "挑战没有生成。Challenge was not created."
            };
            sync_session(session, state::get_signal(PHASE_SIGNAL), notice);
        }
        OP_SHARED_APP => {
            shared_launch::spawn_detached(
                "app",
                &shared_launch::configured_path(
                    "W3COS_SHARED_APP",
                    "/usr/share/w3cos/apps/shared-hello/shared-hello.w3app",
                ),
            );
            state::set_text_signal(
                NOTICE_SIGNAL,
                "已启动共享运行时应用。Launched the shared-runtime app.",
            );
        }
        OP_SHARED_PAGE => {
            shared_launch::spawn_detached(
                "page",
                &shared_launch::configured_path(
                    "W3COS_SHARED_PAGE",
                    "/usr/share/w3cos/apps/shared-hello/page.html",
                ),
            );
            state::set_text_signal(
                NOTICE_SIGNAL,
                "已把网页载入页面 worker。Loaded the page in the page worker.",
            );
        }
        OP_RUN_TERMINAL => run_terminal_from_button(session),
        OP_L2_TOGGLE => toggle_l2(session),
        OP_SHARED_PLUGIN => {
            shared_launch::spawn_detached(
                "plugin",
                &shared_launch::configured_path(
                    "W3COS_SHARED_PLUGIN",
                    "/usr/share/w3cos/apps/shared-counter/libshared_counter.so",
                ),
            );
            state::set_text_signal(
                NOTICE_SIGNAL,
                "已启动 C 插件应用。Launched the C plugin app.",
            );
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
    state::set_text_signal(RECEIPT_SIGNAL, session.receipt_log_tail(12));
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
        push_toast("桌面保持显示。The desktop stays put.");
        return;
    }
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    state::set_signal(OPEN_MASK_SIGNAL, open | bit);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    state::set_signal(MINIMIZED_MASK_SIGNAL, minimized | bit);
    if state::get_signal(APP_SIGNAL) == id {
        state::set_signal(APP_SIGNAL, 0);
    }
    let name = ShellApp::from_signal(id).bilingual();
    push_toast(&format!("已最小化 {name}。Minimized {name}."));
    remember_windows();
}

fn close_window(id: i64) {
    let bit = app_bit(id);
    if bit == 0 {
        push_toast("桌面不能关闭。The desktop has no window to close.");
        return;
    }
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    state::set_signal(OPEN_MASK_SIGNAL, open & !bit);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    state::set_signal(MINIMIZED_MASK_SIGNAL, minimized & !bit);
    if state::get_signal(APP_SIGNAL) == id {
        state::set_signal(APP_SIGNAL, 0);
    }
    let name = ShellApp::from_signal(id).bilingual();
    push_toast(&format!("已关闭 {name}。Closed {name}."));
    remember_windows();
}

fn toggle_window_maximize(id: i64) {
    let (width, height) = stage_bounds();
    if !windows::toggle_maximize(id, width, height) {
        push_toast("桌面不能最大化。The desktop stays put.");
        return;
    }
    focus_app(id);
    let name = ShellApp::from_signal(id).bilingual();
    let verb = if windows::is_maximized(id) {
        "已最大化"
    } else {
        "已还原"
    };
    push_toast(&format!("{verb} {name}。"));
}

fn stage_bounds() -> (f32, f32) {
    let mut width: f32 = 1200.0;
    if state::get_signal(TASKS_SIGNAL) != 0 {
        width -= 300.0;
    }
    if state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0 {
        width -= 300.0;
    }
    (width.max(480.0), 540.0)
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
            let name = ShellApp::from_signal(id).bilingual();
            push_toast(&format!("切换到 {name}。Switched to {name}."));
        }
        Some(_) => push_toast("没有其他打开的窗口。No other open window."),
        None => push_toast("没有打开的窗口。No open window."),
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
            "L2 已关闭。没有启动模型。L2 is off. No model was started.",
        );
        return;
    }
    if l2_env_configured() {
        session.set_l2_enabled(true);
        sync_session(
            session,
            state::get_signal(PHASE_SIGNAL),
            "L2 已打开。改动仍要先确认。L2 is on. Confirm still gates every change.",
        );
        return;
    }
    sync_session(
        session,
        state::get_signal(PHASE_SIGNAL),
        "L2 保持关闭。先设置 W3COS_L2，这个开关不会启动推理。L2 stays off until W3COS_L2 is set.",
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
        Some("receipts") => state::set_signal(RECEIPTS_OPEN_SIGNAL, 1),
        Some("windows") => {
            state::set_signal(OPEN_MASK_SIGNAL, app_bit(1) | app_bit(2) | app_bit(5));
            state::set_signal(MINIMIZED_MASK_SIGNAL, app_bit(5));
            focus_app(1);
            push_toast("浏览器已最小化。Browser is minimized.");
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
    apply_screen_override();
    boot_session_once();
    let current = state::get_signal(APP_SIGNAL);
    let night = state::get_signal(NIGHT_SIGNAL) != 0;
    let tasks_open = state::get_signal(TASKS_SIGNAL) != 0;
    let receipts_open = state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0;

    let mut workspace = vec![build_stage()];
    if tasks_open {
        workspace.push(build_task_center());
    }
    if receipts_open {
        workspace.push(build_receipts_viewer());
    }
    let mut column = vec![
        chrome_layer(build_context_header(current, night)),
        chrome_layer(build_intent_bar()),
        Component::row(
            Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_height: Dimension::Px(0.0),
                overflow: Overflow::Hidden,
                gap: 0.0,
                align_items: AlignItems::Stretch,
                ..Style::default()
            },
            workspace,
        ),
    ];
    let toast = state::get_signal_text(TOAST_SIGNAL);
    if !toast.is_empty() {
        column.push(chrome_layer(build_toast(&toast)));
    }
    if state::get_signal(LAUNCHER_SIGNAL) != 0 {
        column.push(chrome_layer(build_launcher()));
    }
    column.push(chrome_layer(build_taskbar(current)));

    Component::column(
        Style {
            width: Dimension::Vw(100.0),
            height: Dimension::Vh(100.0),
            overflow: Overflow::Hidden,
            background: Color::from_hex(if night { "#15202c" } else { "#1a2836" }),
            gap: 0.0,
            ..Style::default()
        },
        column,
    )
}

const CHROME_Z: i32 = 1_000_000;

fn chrome_layer(mut component: Component) -> Component {
    component.style.position = Position::Relative;
    component.style.z_index = CHROME_Z;
    component
}

fn build_stage() -> Component {
    let current = state::get_signal(APP_SIGNAL);
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    let mut ids: Vec<i64> = (1..=6)
        .filter(|id| open & app_bit(*id) != 0 && minimized & app_bit(*id) == 0)
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
    let border = if focused { "#9eb6ff" } else { "#2a3e52" };
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
            border_color: Color::from_hex(border),
            background: Color::from_hex("#0f0f1a"),
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
            background: Color::from_hex("#2a3e52"),
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
            background: Color::from_hex("#d5dee8"),
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
    let mut children = vec![build_desktop_icons(), build_system_info(), build_branding()];
    if std::env::var_os("W3COS_SHARED_LAUNCHER").is_some() {
        children.insert(1, build_shared_launcher());
    }
    if night {
        children.push(Component::text(
            "Night mode is on",
            Style {
                font_size: 14.0,
                color: Color::from_hex("#a29bfe"),
                ..Style::default()
            },
        ));
    }
    Component::column(
        Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_height: Dimension::Px(0.0),
            overflow: Overflow::Hidden,
            background: Color::from_hex(if night { "#1a3048" } else { "#24506e" }),
            padding: Edges::all(28.0),
            gap: 20.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..Style::default()
        },
        children,
    )
}

fn build_desktop_icons() -> Component {
    let apps = [
        ("F", "Files", "#3498db", 1),
        ("T", "Terminal", "#2ecc71", 2),
        ("S", "Settings", "#9b59b6", 3),
        ("A", "AI Agent", "#e74c3c", 4),
        ("B", "Browser", "#f39c12", 5),
        ("E", "Editor", "#1abc9c", 6),
    ];
    let icons: Vec<Component> = apps
        .iter()
        .map(|(l, label, c, id)| {
            Component::column(
                Style {
                    align_items: AlignItems::Center,
                    gap: 12.0,
                    ..Style::default()
                },
                vec![
                    Component::button_with_click(
                        *l,
                        Style {
                            width: Dimension::Px(80.0),
                            height: Dimension::Px(80.0),
                            background: Color::from_hex(c),
                            border_radius: 16.0,
                            font_size: 32.0,
                            font_weight: 700,
                            color: Color::WHITE,
                            ..Style::default()
                        },
                        invoke(focus_code(*id)),
                    ),
                    Component::text(
                        *label,
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#c0c0d0"),
                            ..Style::default()
                        },
                    ),
                ],
            )
        })
        .collect();
    Component::row(
        Style {
            gap: 40.0,
            flex_wrap: FlexWrap::Wrap,
            justify_content: JustifyContent::Center,
            ..Style::default()
        },
        icons,
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
                "Shared Hello",
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
                "Web VM",
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
                "Counter",
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

fn build_system_info() -> Component {
    Component::row(
        Style {
            gap: 24.0,
            justify_content: JustifyContent::Center,
            flex_wrap: FlexWrap::Wrap,
            ..Style::default()
        },
        vec![
            info_card("CPU", "23%", "#00b894"),
            info_card("Memory", "5.4 / 8 GB", "#fdcb6e"),
            info_card("Storage", "205 / 512 GB", "#74b9ff"),
            info_card("Network", "84 Mbps", "#a29bfe"),
        ],
    )
}

fn info_card(title: &str, value: &str, accent: &str) -> Component {
    Component::column(
        Style {
            padding: Edges::all(20.0),
            background: Color::from_hex("#2c4158"),
            border_radius: 12.0,
            gap: 8.0,
            min_width: Dimension::Px(160.0),
            flex_grow: 1.0,
            ..Style::default()
        },
        vec![
            Component::text(
                title,
                Style {
                    font_size: 13.0,
                    color: Color::from_hex("#d5dee8"),
                    ..Style::default()
                },
            ),
            Component::text(
                value,
                Style {
                    font_size: 22.0,
                    color: Color::from_hex("#f0f0ff"),
                    font_weight: 700,
                    ..Style::default()
                },
            ),
            Component::row(
                Style {
                    height: Dimension::Px(6.0),
                    border_radius: 3.0,
                    background: Color::from_hex("#1e1e38"),
                    ..Style::default()
                },
                vec![Component::column(
                    Style {
                        width: Dimension::Percent(40.0),
                        height: Dimension::Px(6.0),
                        border_radius: 3.0,
                        background: Color::from_hex(accent),
                        ..Style::default()
                    },
                    vec![],
                )],
            ),
        ],
    )
}

fn build_branding() -> Component {
    Component::column(
        Style {
            align_items: AlignItems::Center,
            gap: 6.0,
            opacity: 0.5,
            ..Style::default()
        },
        vec![
            Component::text(
                "W3C OS",
                Style {
                    font_size: 20.0,
                    color: Color::from_hex("#6c5ce7"),
                    font_weight: 700,
                    ..Style::default()
                },
            ),
            Component::text(
                "Native Desktop Shell - TypeScript compiled to native binary",
                Style {
                    font_size: 13.0,
                    color: Color::from_hex("#808090"),
                    ..Style::default()
                },
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
                            nav_button("主文件夹 Home", folder == 0, OP_FILES_HOME),
                            nav_button("文档 Documents", folder == 1, OP_FILES_DOCS),
                            nav_button("图片 Pictures", folder == 2, OP_FILES_PICS),
                            nav_button("下载 Downloads", folder == 3, OP_FILES_DOWN),
                            nav_button("音乐 Music", folder == 4, OP_FILES_MUSIC),
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
                ],
            ),
        ],
    )
}

fn folder_title(folder: i64) -> (&'static str, &'static str) {
    match folder {
        1 => ("文档 Documents", "主文件夹 / 文档"),
        2 => ("图片 Pictures", "主文件夹 / 图片"),
        3 => ("下载 Downloads", "主文件夹 / 下载"),
        4 => ("音乐 Music", "主文件夹 / 音乐"),
        5 => ("projects", "主文件夹 / projects"),
        6 => ("w3cos", "主文件夹 / w3cos"),
        _ => ("文件 Files", "主文件夹 Home"),
    }
}

fn folder_summary(folder: i64) -> &'static str {
    match folder {
        1 => "2 items",
        2 => "2 items",
        3 => "1 item",
        4 => "0 items",
        5 => "2 items",
        6 => "2 items",
        _ => "6 items",
    }
}

fn folder_rows(folder: i64) -> Vec<Component> {
    match folder {
        1 => vec![
            file_row("notes.txt", "Text", "1.2 KB", None),
            file_row("design.md", "Markdown", "8.4 KB", None),
        ],
        2 => vec![
            file_row("screenshot.png", "PNG Image", "245 KB", None),
            file_row("wallpaper.png", "PNG Image", "1.1 MB", None),
        ],
        3 => vec![file_row("fetch-l2-models.sh", "Script", "2.0 KB", None)],
        4 => vec![Component::text(
            "这个文件夹是空的。This folder is empty.",
            Style {
                font_size: 14.0,
                color: Color::from_hex("#d5dee8"),
                padding: Edges::all(12.0),
                ..Style::default()
            },
        )],
        5 => vec![
            file_row("README.md", "Markdown", "4.2 KB", None),
            file_row("Cargo.toml", "TOML", "1.1 KB", None),
        ],
        6 => vec![
            file_row("app.tsx", "TypeScript", "2.8 KB", None),
            file_row("shell.rs", "Rust", "18 KB", None),
        ],
        _ => vec![
            file_row("projects", "Folder", "--", Some(OP_FILES_PROJECTS)),
            file_row("w3cos", "Folder", "--", Some(OP_FILES_W3COS)),
            file_row("README.md", "Markdown", "4.2 KB", None),
            file_row("Cargo.toml", "TOML", "1.1 KB", None),
            file_row("app.tsx", "TypeScript", "2.8 KB", None),
            file_row("screenshot.png", "PNG Image", "245 KB", None),
        ],
    }
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
            border_radius: 6.0,
            padding: Edges::xy(10.0, 8.0),
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
    let name_node = if let Some(code) = open {
        Component::button_with_click(name, name_style, invoke(code))
    } else {
        Component::text(name, name_style)
    };
    Component::row(
        Style {
            padding: Edges::xy(8.0, 10.0),
            gap: 16.0,
            border_radius: 4.0,
            align_items: AlignItems::Center,
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
            app_titlebar("终端 Terminal", "#1c8f62", 2),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    padding: Edges::all(16.0),
                    gap: 10.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        "允许 echo、pwd、help。运行时申请一次性的 w3cos-process。",
                        Style {
                            font_size: 16.0,
                            font_weight: 700,
                            color: Color::from_hex("#f4f7fb"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "Allowlist: echo, pwd, help. Run asks for a one-shot w3cos-process grant.",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#d5dee8"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "w3cos shell ready",
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
                            Component::text_input(
                                state::get_signal_text(TERMINAL_SIGNAL),
                                "help",
                                Style {
                                    font_size: 14.0,
                                    color: Color::from_hex("#f4f7fb"),
                                    background: Color::from_hex("#1b2c3e"),
                                    flex_grow: 1.0,
                                    ..Style::default()
                                },
                            ),
                            chrome_button("运行 Run", OP_RUN_TERMINAL, "#1c8f62"),
                        ],
                    ),
                    Component::text(
                        {
                            let output = state::get_signal_text(TERM_OUT_SIGNAL);
                            if output.is_empty() {
                                "允许 echo、pwd、help。需要一次性的 w3cos-process。".to_string()
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
            app_titlebar("设置 Settings", "#5b4fe0", 3),
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
                            nav_button("本机 Local", tab == 0, OP_SETTINGS_LOCAL),
                            nav_button("身份 Identity", tab == 1, OP_SETTINGS_IDENTITY),
                            nav_button("权限 Permissions", tab == 2, OP_SETTINGS_BRIDGE),
                            nav_button("L2 模型", tab == 3, OP_SETTINGS_L2),
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
        _ => settings_local_page(),
    }
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
        "开 On"
    } else {
        "关 Off"
    };
    let mut rows = settings_heading(
        "本机设置 · Local preferences",
        "没有账号，也没有链。夜间模式仍走提议和确认。",
    );
    rows.push(settings_row("夜间模式 Night", night));
    rows.push(chrome_button(
        "切换夜间 Toggle night",
        OP_CHIP_NIGHT,
        "#3d5570",
    ));
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
        "WebAuthn 仪式还没做。当前是 did:key 占位，换成真钥匙时不改已有收据。".to_string()
    } else {
        enroll_owned
    };
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let mut rows = settings_heading(
        "身份 Identity",
        "口令经 KDF 保存，还不是 passkey。锁定后不能确认改动。",
    );
    rows.push(settings_row("主体 Subject", &subject_label));
    rows.push(Component::text(
        enroll_label,
        Style {
            font_size: 13.0,
            color: Color::from_hex("#d5dee8"),
            ..Style::default()
        },
    ));
    rows.push(chrome_button(
        "生成注册挑战 Challenge",
        OP_ENROLL,
        "#3d5570",
    ));
    rows.push(settings_row("身份 Identity", "PBKDF2 · not a passkey"));
    rows.push(settings_row(
        "状态 Status",
        if unlocked {
            "已解锁 Unlocked"
        } else {
            "已锁定 Locked"
        },
    ));
    rows.push(chrome_button(
        if unlocked {
            "锁定 Lock"
        } else {
            "解锁 Unlock"
        },
        if unlocked { OP_LOCK } else { OP_UNLOCK },
        if unlocked { "#3d5570" } else { "#9a3412" },
    ));
    rows.push(settings_row("收据存储 Receipts", &storage_label));
    rows.push(chrome_button(
        "打开收据本 Ledger",
        OP_TOGGLE_RECEIPTS,
        "#3d5570",
    ));
    rows
}

fn settings_permissions_page() -> Vec<Component> {
    let bridge = std::env::var("W3COS_AI_PERMISSIONS").unwrap_or_default();
    let bridge = match bridge.trim() {
        "" | "observer" => "observer（HTTP 默认，本页不能调高）",
        "interactive" => "interactive（来自环境变量）",
        "system" => "system（来自环境变量）",
        _ => "observer（无法识别的值已忽略）",
    };
    let mut rows = settings_heading(
        "权限 Permissions",
        "HTTP 桥的权限只看 W3COS_AI_PERMISSIONS。这个页面不会把它调高。",
    );
    rows.push(settings_row("HTTP 桥 Bridge", bridge));
    rows.push(settings_row(
        "本机会话 Shell",
        "interactive · 确认后才授予一次性 act / process",
    ));
    rows.push(settings_row(
        "启动时授予 Boot grants",
        "w3cos-accessibility · display-capture",
    ));
    rows.push(settings_row(
        "不会常驻 Not standing",
        "w3cos-agent-act · w3cos-process",
    ));
    rows
}

fn settings_l2_page() -> Vec<Component> {
    let env_flag = std::env::var("W3COS_L2").unwrap_or_else(|_| "未设置".into());
    let env_flag = if env_flag.trim().is_empty() {
        "未设置".to_string()
    } else {
        env_flag
    };
    let model = std::env::var("W3COS_L2_MODEL").unwrap_or_else(|_| {
        "未设置。打开 L2 时默认 models/qwen2.5-1.5b-instruct-q4_k_m.gguf".into()
    });
    let endpoint = std::env::var("W3COS_L2_URL").unwrap_or_else(|_| "未设置，不连接".into());
    let on = session_l2_on();
    let mut rows = settings_heading(
        "L2 模型",
        "默认关闭。开关只改本会话，不会启动 llama-server，也不会下载权重。",
    );
    rows.push(settings_row(
        "当前 Current",
        if on { "开 On" } else { "关 Off" },
    ));
    rows.push(settings_row("环境 W3COS_L2", &env_flag));
    rows.push(settings_row("模型路径 Model", &model));
    rows.push(settings_row("端点 Endpoint", &endpoint));
    rows.push(chrome_button(
        if on {
            "关闭 L2 Turn L2 off"
        } else {
            "打开 L2 Turn L2 on"
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

fn build_context_header(current: i64, night: bool) -> Component {
    let shared = state::get_signal(CONTEXT_SIGNAL) != 0;
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let app = if shared {
        ShellApp::from_signal(current).bilingual()
    } else {
        "已隐藏 Hidden"
    };
    Component::row(
        Style {
            flex_shrink: 0.0,
            padding: Edges::xy(16.0, 8.0),
            gap: 8.0,
            background: Color::from_hex("#24384c"),
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![
            Component::text(
                "上下文 Context",
                Style {
                    font_size: 13.0,
                    font_weight: 700,
                    color: ink(),
                    ..Style::default()
                },
            ),
            chrome_button(
                if shared {
                    format!("{app}  ×")
                } else {
                    "恢复上下文 Restore".to_string()
                },
                if shared { OP_REVOKE } else { OP_RESTORE },
                "#3d5570",
            ),
            Component::text(
                if night {
                    "夜间 开 Night on"
                } else {
                    "夜间 关 Night off"
                },
                Style {
                    font_size: 12.0,
                    color: muted_ink(),
                    background: Color::from_hex("#31475e"),
                    border_radius: 8.0,
                    padding: Edges::xy(10.0, 6.0),
                    ..Style::default()
                },
            ),
            Component::text(
                if unlocked {
                    "已解锁 Unlocked"
                } else {
                    "已锁定 Locked"
                },
                Style {
                    font_size: 12.0,
                    color: muted_ink(),
                    background: Color::from_hex("#31475e"),
                    border_radius: 8.0,
                    padding: Edges::xy(10.0, 6.0),
                    ..Style::default()
                },
            ),
            chrome_button(
                if state::get_signal(TASKS_SIGNAL) != 0 {
                    "收起任务 Hide"
                } else {
                    "任务 Tasks"
                },
                OP_TOGGLE_TASKS,
                "#3d5570",
            ),
            Component::text(
                "实时 Live",
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#b7f0d8"),
                    ..Style::default()
                },
            ),
        ],
    )
}

fn build_intent_bar() -> Component {
    let phase = state::get_signal(PHASE_SIGNAL);
    let mut input_style = Style {
        flex_grow: 1.0,
        font_size: 15.0,
        color: Color::from_hex("#1a2836"),
        background: Color::from_hex("#f4f7fb"),
        border_radius: 8.0,
        padding: Edges::xy(12.0, 8.0),
        ..Style::default()
    };
    input_style.custom_properties = Some(HashMap::from([(
        "--w3cos-intent-submit".to_string(),
        OP_PROPOSE.to_string(),
    )]));
    let mut rows = vec![
        Component::row(
            Style {
                gap: 8.0,
                align_items: AlignItems::Center,
                ..Style::default()
            },
            vec![
                Component::text_input(
                    state::get_signal_text(DRAFT_SIGNAL),
                    INTENT_INPUT_PLACEHOLDER,
                    input_style,
                ),
                chrome_button("提议 Propose", OP_PROPOSE, "#5b4fe0"),
            ],
        ),
        Component::row(
            Style {
                gap: 8.0,
                align_items: AlignItems::Center,
                ..Style::default()
            },
            vec![
                chrome_button("打开文件 Files", OP_CHIP_FILES, "#3d5570"),
                chrome_button("打开终端 Terminal", OP_CHIP_TERMINAL, "#3d5570"),
                chrome_button("打开设置 Settings", OP_CHIP_SETTINGS, "#3d5570"),
                chrome_button("夜间模式 Night", OP_CHIP_NIGHT, "#3d5570"),
                chrome_button("应用列表 Apps", OP_CHIP_APPS, "#3d5570"),
                chrome_button("我在哪 Where", OP_CHIP_CONTEXT, "#3d5570"),
            ],
        ),
    ];
    if phase == 1 {
        rows.push(confirm_card());
    } else if phase == 3 {
        rows.push(result_card("规划中 Planning · 模型在后台线程"));
    } else if phase == 2 {
        let notice = state::get_signal_text(NOTICE_SIGNAL);
        if !notice.is_empty() {
            rows.push(result_card(&notice));
        }
    }
    Component::column(
        Style {
            flex_shrink: 0.0,
            padding: Edges::xy(16.0, 10.0),
            gap: 8.0,
            background: Color::from_hex("#1e3144"),
            ..Style::default()
        },
        rows,
    )
}

fn confirm_card() -> Component {
    let mut rows = Vec::new();
    let tier = state::get_signal_text(TIER_SIGNAL);
    if tier == "L1" || tier == "L2" {
        rows.push(Component::row(
            Style {
                padding: Edges::xy(8.0, 4.0),
                background: Color::from_hex(if tier == "L2" { "#1d4e89" } else { "#9a3412" }),
                border_radius: 6.0,
                align_self: AlignSelf::FlexStart,
                ..Style::default()
            },
            vec![Component::text(
                if tier == "L2" {
                    "L2 规划 Planner"
                } else {
                    "L1 本地 Local"
                },
                Style {
                    font_size: 12.0,
                    font_weight: 700,
                    color: Color::WHITE,
                    ..Style::default()
                },
            )],
        ));
    }
    rows.push(Component::text(
        "确认后才会执行 · Confirm to run",
        Style {
            font_size: 12.0,
            font_weight: 700,
            color: Color::from_hex("#ffe08a"),
            ..Style::default()
        },
    ));
    rows.push(Component::text(
        state::get_signal_text(SUMMARY_SIGNAL),
        Style {
            font_size: 16.0,
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
    rows.push(Component::row(
        Style {
            gap: 8.0,
            ..Style::default()
        },
        vec![
            chrome_button("确认 Confirm", OP_CONFIRM, "#1c8f62"),
            chrome_button("取消 Cancel", OP_CANCEL, "#3d5570"),
        ],
    ));
    Component::column(
        Style {
            gap: 6.0,
            padding: Edges::all(12.0),
            background: Color::from_hex("#31475e"),
            border_radius: 8.0,
            ..Style::default()
        },
        rows,
    )
}

fn result_card(notice: &str) -> Component {
    Component::row(
        Style {
            gap: 8.0,
            padding: Edges::xy(12.0, 8.0),
            background: Color::from_hex("#31475e"),
            border_radius: 8.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..Style::default()
        },
        vec![
            Component::text(
                format!("结果 Result · {notice}"),
                Style {
                    font_size: 13.0,
                    color: ink(),
                    flex_grow: 1.0,
                    ..Style::default()
                },
            ),
            chrome_button("关闭 Close", OP_CANCEL, "#3d5570"),
        ],
    )
}

fn build_task_center() -> Component {
    let phase = state::get_signal(PHASE_SIGNAL);
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let (status, status_color) = match phase {
        1 => ("等待确认 Awaiting confirm", "#ffe08a"),
        2 => ("已完成 Done", "#b7f0d8"),
        3 => ("规划中 Planning", "#9eb6ff"),
        _ => ("空闲 Idle", "#d5dee8"),
    };
    let mut body = vec![
        Component::text(
            "任务中心 Tasks",
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
                format!(
                    "{subject_label} · {} · 口令经 KDF 保存，还不是 passkey",
                    if unlocked { "stub unlocked" } else { "locked" }
                )
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
            "已锁定。确认前请先解锁。Locked — unlock before confirm.",
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
            "锁定 Lock"
        } else {
            "解锁才能执行 Unlock to run"
        },
        if unlocked { OP_LOCK } else { OP_UNLOCK },
        if unlocked { "#3d5570" } else { "#9a3412" },
    ));
    let steps = state::get_signal_text(STEPS_SIGNAL);
    if !steps.is_empty() {
        body.push(Component::text(
            "步骤 Steps",
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
        "收据 Receipts",
        Style {
            font_size: 14.0,
            font_weight: 700,
            color: ink(),
            ..Style::default()
        },
    ));
    body.push(chrome_button(
        "收据本 Ledger",
        OP_TOGGLE_RECEIPTS,
        "#3d5570",
    ));
    let log = state::get_signal_text(RECEIPT_SIGNAL);
    if log.is_empty() || log == "No receipts yet." {
        body.push(Component::text(
            "还没有收据。No receipts yet.",
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for line in log.lines().take(8) {
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
    if phase == 1 || phase == 3 {
        body.push(chrome_button("停止 Stop", OP_CANCEL, "#a33b45"));
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
        Some("script") => "L2 脚本夹具 · 无权重 · 后台线程".to_string(),
        Some("1") | Some("true") => {
            let model = std::env::var("W3COS_L2_MODEL").unwrap_or_else(|_| "local".into());
            format!("L2 {model} · 后台线程 · 确认后执行")
        }
        _ => {
            if state::get_signal_text(STORAGE_SIGNAL) == "encrypted-file" {
                "本地 L0/L1 · 无模型 · 加密文件".to_string()
            } else {
                "本地 L0/L1 · 无模型 · 仅内存".to_string()
            }
        }
    }
}

fn chrome_button(label: impl Into<String>, code: u8, background: &str) -> Component {
    let action = if code == OP_PROPOSE {
        EventAction::Invoke {
            code,
            input_placeholder: INTENT_INPUT_PLACEHOLDER.to_string(),
        }
    } else {
        invoke(code)
    };
    Component::button_with_click(
        label,
        Style {
            background: Color::from_hex(background),
            border_radius: 8.0,
            font_size: 13.0,
            font_weight: 700,
            color: Color::WHITE,
            padding: Edges::xy(12.0, 8.0),
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
            app_titlebar("助手 Agent", "#e07a5f", 4),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    padding: Edges::all(24.0),
                    gap: 12.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        "会话在右侧任务中心。这个窗口只是同一条收据的阅读页。",
                        Style {
                            font_size: 16.0,
                            font_weight: 700,
                            color: ink(),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "The live session stays in the task center beside this window.",
                        Style {
                            font_size: 14.0,
                            color: muted_ink(),
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
            app_titlebar("Browser", "#f39c12", 5),
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
                    browser_tab("Docs", false),
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
                        "The AI-Native Operating System",
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
                                "Welcome to the W3C OS Browser",
                                Style {
                                    font_size: 16.0,
                                    color: Color::from_hex("#f0f0ff"),
                                    font_weight: 600,
                                    ..Style::default()
                                },
                            ),
                            Component::text(
                                "Applications are rendered natively using the W3C DOM model — no Chromium, no V8. TypeScript is compiled to native Rust binaries.",
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
                                        "Get Started",
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
                                        "Documentation",
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
            app_titlebar("Editor", "#1abc9c", 6),
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
                        "File",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "Edit",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "View",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "Terminal",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "Help",
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
                                "EXPLORER",
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

fn window_button(label: &str, code: u8, color: &str, app_id: i64) -> Component {
    let mut button = Component::button_with_click(
        label,
        Style {
            font_size: 12.0,
            color: Color::from_hex(color),
            background: Color::from_hex("#2a2a3e"),
            border_radius: 4.0,
            padding: Edges::xy(8.0, 4.0),
            ..Style::default()
        },
        invoke(code),
    );
    button.style.custom_properties = Some(win_props(app_id, None));
    button
}

fn taskbar_icon(letter: &str, color: &str, id: i64, current: i64) -> Component {
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL);
    let bit = app_bit(id);
    let focused = current == id;
    let is_open = open & bit != 0;
    let is_mini = minimized & bit != 0;
    let opacity = if focused || is_open {
        1.0
    } else if is_mini {
        0.55
    } else {
        0.85
    };
    Component::button_with_click(
        letter,
        Style {
            width: Dimension::Px(36.0),
            height: Dimension::Px(36.0),
            background: Color::from_hex(color),
            border_radius: 8.0,
            font_size: 16.0,
            font_weight: 700,
            color: Color::WHITE,
            opacity,
            border_bottom_width: Some(if focused {
                3.0
            } else if is_open {
                2.0
            } else {
                0.0
            }),
            border_bottom_color: Some(Color::from_hex(if focused { "#f4f7fb" } else { "#d5dee8" })),
            ..Style::default()
        },
        invoke(focus_code(id)),
    )
}

fn app_titlebar(title: &str, accent: &str, app_id: i64) -> Component {
    let max_label = if windows::is_maximized(app_id) {
        "回"
    } else {
        "[]"
    };
    let mut bar = Component::row(
        Style {
            height: Dimension::Px(40.0),
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
                    window_button("--", OP_MINIMIZE, "#808090", app_id),
                    window_button(max_label, OP_MAXIMIZE, "#808090", app_id),
                    window_button("X", OP_CLOSE, "#e94560", app_id),
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

fn build_taskbar(current: i64) -> Component {
    let open = state::get_signal(OPEN_MASK_SIGNAL);
    let open_count = (1..=6).filter(|id| open & app_bit(*id) != 0).count();
    let center_text = format!(
        "{} · {open_count} open",
        ShellApp::from_signal(current).bilingual()
    );
    let unlocked = state::get_signal(UNLOCKED_SIGNAL) != 0;
    let launcher_open = state::get_signal(LAUNCHER_SIGNAL) != 0;
    Component::row(
        Style {
            height: Dimension::Px(56.0),
            flex_shrink: 0.0,
            background: Color::from_hex("#162433"),
            padding: Edges::xy(10.0, 6.0),
            gap: 8.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..Style::default()
        },
        vec![
            Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    Component::button_with_click(
                        if launcher_open {
                            "关闭启动器"
                        } else {
                            "W3C Apps"
                        },
                        Style {
                            background: Color::from_hex(if launcher_open {
                                "#3d5570"
                            } else {
                                "#6c5ce7"
                            }),
                            border_radius: 8.0,
                            font_size: 14.0,
                            font_weight: 600,
                            color: Color::WHITE,
                            padding: Edges::xy(14.0, 8.0),
                            ..Style::default()
                        },
                        invoke(OP_LAUNCHER),
                    ),
                    taskbar_icon("F", "#3498db", 1, current),
                    taskbar_icon("T", "#2ecc71", 2, current),
                    taskbar_icon("S", "#9b59b6", 3, current),
                    taskbar_icon("A", "#e74c3c", 4, current),
                    taskbar_icon("B", "#f39c12", 5, current),
                    taskbar_icon("E", "#1abc9c", 6, current),
                ],
            ),
            Component::text(
                center_text,
                Style {
                    font_size: 14.0,
                    color: Color::from_hex("#d5dee8"),
                    ..Style::default()
                },
            ),
            Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    chrome_button(
                        if unlocked { "锁定" } else { "解锁" },
                        if unlocked { OP_LOCK } else { OP_UNLOCK },
                        if unlocked { "#3d5570" } else { "#9a3412" },
                    ),
                    chrome_button(
                        if state::get_signal(RECEIPTS_OPEN_SIGNAL) != 0 {
                            "收起收据"
                        } else {
                            "收据"
                        },
                        OP_TOGGLE_RECEIPTS,
                        "#3d5570",
                    ),
                    Component::text(
                        "Online",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#00b894"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "14:32",
                        Style {
                            font_size: 14.0,
                            color: Color::from_hex("#d0d0e0"),
                            background: Color::from_hex("#1c1c34"),
                            border_radius: 8.0,
                            padding: Edges::xy(12.0, 6.0),
                            ..Style::default()
                        },
                    ),
                ],
            ),
        ],
    )
}

struct LaunchApp {
    id: i64,
    letter: &'static str,
    zh: &'static str,
    en: &'static str,
    color: &'static str,
}

const LAUNCH_APPS: [LaunchApp; 7] = [
    LaunchApp {
        id: 0,
        letter: "D",
        zh: "桌面",
        en: "Desktop",
        color: "#6c5ce7",
    },
    LaunchApp {
        id: 1,
        letter: "F",
        zh: "文件",
        en: "Files",
        color: "#3498db",
    },
    LaunchApp {
        id: 2,
        letter: "T",
        zh: "终端",
        en: "Terminal",
        color: "#2ecc71",
    },
    LaunchApp {
        id: 3,
        letter: "S",
        zh: "设置",
        en: "Settings",
        color: "#9b59b6",
    },
    LaunchApp {
        id: 4,
        letter: "A",
        zh: "助手",
        en: "Agent",
        color: "#e74c3c",
    },
    LaunchApp {
        id: 5,
        letter: "B",
        zh: "浏览器",
        en: "Browser",
        color: "#f39c12",
    },
    LaunchApp {
        id: 6,
        letter: "E",
        zh: "编辑器",
        en: "Editor",
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
    search_style.custom_properties = Some(HashMap::from([(
        "--w3cos-intent-submit".to_string(),
        OP_LAUNCHER_SEARCH.to_string(),
    )]));
    let needle = query.trim().to_lowercase();
    let mut tiles = Vec::new();
    for app in LAUNCH_APPS {
        let hay = format!("{} {} {}", app.zh, app.en, app.letter).to_lowercase();
        if !needle.is_empty() && !hay.contains(&needle) {
            continue;
        }
        tiles.push(launcher_tile(app));
    }
    if tiles.is_empty() {
        tiles.push(Component::text(
            "没有匹配的应用。No matching app.",
            Style {
                font_size: 13.0,
                color: Color::from_hex("#d5dee8"),
                ..Style::default()
            },
        ));
    }
    let query_label = if query.trim().is_empty() {
        "全部应用".to_string()
    } else {
        format!("搜索：{}", query.trim())
    };
    Component::column(
        Style {
            flex_shrink: 0.0,
            background: Color::from_hex("#1b2c3e"),
            padding: Edges::all(12.0),
            gap: 8.0,
            ..Style::default()
        },
        vec![
            Component::row(
                Style {
                    gap: 8.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    Component::text_input(query.clone(), LAUNCHER_PLACEHOLDER, search_style),
                    Component::button_with_click(
                        "搜索 Search",
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
                            input_placeholder: LAUNCHER_PLACEHOLDER.to_string(),
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
        ],
    )
}

fn launcher_tile(app: LaunchApp) -> Component {
    let current = state::get_signal(APP_SIGNAL) == app.id;
    let open = state::get_signal(OPEN_MASK_SIGNAL) & app_bit(app.id) != 0;
    let minimized = state::get_signal(MINIMIZED_MASK_SIGNAL) & app_bit(app.id) != 0;
    let status = if current {
        "在前台"
    } else if minimized {
        "最小化"
    } else if open {
        "已打开"
    } else {
        ""
    };
    let label = if status.is_empty() {
        format!("{}  {} {}", app.letter, app.zh, app.en)
    } else {
        format!("{}  {} {} · {status}", app.letter, app.zh, app.en)
    };
    Component::button_with_click(
        label,
        Style {
            background: Color::from_hex(app.color),
            border_radius: 8.0,
            font_size: 13.0,
            font_weight: 700,
            color: Color::WHITE,
            padding: Edges::xy(12.0, 8.0),
            ..Style::default()
        },
        invoke(focus_code(app.id)),
    )
}

fn build_toast(text: &str) -> Component {
    Component::row(
        Style {
            flex_shrink: 0.0,
            padding: Edges::xy(16.0, 8.0),
            gap: 8.0,
            background: Color::from_hex("#1d4e89"),
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![
            Component::text(
                "通知",
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
            chrome_button("知道了", OP_DISMISS_TOAST, "#3d5570"),
        ],
    )
}

fn build_receipts_viewer() -> Component {
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
    let log = state::get_signal_text(RECEIPT_SIGNAL);
    let mut body = vec![
        Component::text(
            "收据本 Receipts",
            Style {
                font_size: 16.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ),
        Component::text(
            format!("{subject_label} · {storage_label}"),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        chrome_button("关闭 Close", OP_TOGGLE_RECEIPTS, "#3d5570"),
    ];
    if log.is_empty() || log == "No receipts yet." {
        body.push(Component::text(
            "还没有收据。No receipts yet.",
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ));
    } else {
        for line in log.lines() {
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
    Component::column(
        Style {
            width: Dimension::Px(300.0),
            flex_shrink: 0.0,
            min_height: Dimension::Px(0.0),
            overflow: Overflow::Auto,
            padding: Edges::all(14.0),
            gap: 8.0,
            background: Color::from_hex("#243047"),
            ..Style::default()
        },
        body,
    )
}
