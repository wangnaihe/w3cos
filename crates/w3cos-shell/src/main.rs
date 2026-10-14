//! W3C OS Desktop Shell — the system-level GUI.
//!
//! Boot sequence: Linux kernel → init → S99w3cos → w3cos-shell (this binary)
//! Click an app icon → the app UI replaces the desktop area.
//! Click "W3C Apps" in taskbar → return to desktop.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_ai_bridge::identity::LocalIdentityStub;
use w3cos_ai_bridge::intent::{
    ExecuteResult, IntentSession, ProposeResult, ShellApp, ShellContext, INTENT_INPUT_PLACEHOLDER,
};
use w3cos_ai_bridge::permissions::AgentPermissions;
use w3cos_runtime::state;
use w3cos_std::color::Color;
use w3cos_std::style::*;
use w3cos_std::{Component, EventAction, Style};

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

fn main() {
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
        w3cos_runtime::enable_ai_bridge(port);
        eprintln!("[AI Bridge] Will start on port {port}");
    }

    state::set_invoke_hook(on_shell_intent);
    w3cos_ai_bridge::server::set_intent_hooks(bridge_propose, bridge_confirm);
    w3cos_runtime::run_app(build_shell).expect("W3C OS Shell crashed");
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
    with_session(|session| sync_session(session, 0, ""));
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
            session.cancel();
            sync_session(session, 0, "");
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
        _ => {}
    });
}

fn apply_propose(session: &mut IntentSession, utterance: &str, ctx: &ShellContext) {
    state::set_text_signal(DRAFT_SIGNAL, utterance);
    match session.propose(utterance, ctx) {
        ProposeResult::Confirm(proposal) => {
            state::set_text_signal(SUMMARY_SIGNAL, proposal.summary);
            state::set_text_signal(IMPACT_SIGNAL, proposal.impact);
            sync_session(session, 1, "");
        }
        ProposeResult::Unrecognized { receipt } => {
            state::set_text_signal(SUMMARY_SIGNAL, "");
            state::set_text_signal(IMPACT_SIGNAL, "");
            sync_session(session, 2, &receipt.outcome);
        }
    }
}

fn apply_confirm(session: &mut IntentSession, ctx: &ShellContext) {
    let Some(effect) = session.confirm(ctx, now_ms()) else {
        sync_session(session, 0, "");
        return;
    };
    apply_effect(&effect);
    sync_session(session, 2, &effect.receipt.outcome);
}

fn apply_effect(effect: &ExecuteResult) {
    if let Some(app) = effect.navigate {
        state::set_signal(APP_SIGNAL, app.signal_value());
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
    state::set_text_signal(RECEIPT_SIGNAL, session.receipt_log_tail(12));
    state::set_text_signal(STORAGE_SIGNAL, session.receipt_storage());
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

fn build_shell() -> Component {
    ensure_shell_signals();
    boot_session_once();
    let current = state::get_signal(APP_SIGNAL);
    let night = state::get_signal(NIGHT_SIGNAL) != 0;
    let tasks_open = state::get_signal(TASKS_SIGNAL) != 0;

    let content = match current {
        0 => build_desktop(),
        1 => build_files_app(),
        2 => build_terminal_app(),
        3 => build_settings_app(),
        4 => build_ai_agent_app(),
        5 => build_browser_app(),
        6 => build_editor_app(),
        _ => build_desktop(),
    };

    Component::column(
        Style {
            width: Dimension::Vw(100.0),
            height: Dimension::Vh(100.0),
            background: Color::from_hex(if night { "#15202c" } else { "#1a2836" }),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            build_context_header(current, night),
            build_intent_bar(),
            Component::row(
                Style {
                    flex_grow: 1.0,
                    gap: 0.0,
                    align_items: AlignItems::Stretch,
                    ..Style::default()
                },
                if tasks_open {
                    vec![content, build_task_center()]
                } else {
                    vec![content]
                },
            ),
            build_taskbar(current),
        ],
    )
}

// ==========================================================================
// Desktop (home screen)
// ==========================================================================

fn build_desktop() -> Component {
    let night = state::get_signal(NIGHT_SIGNAL) != 0;
    let mut children = vec![build_desktop_icons(), build_system_info(), build_branding()];
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
            background: Color::from_hex(if night { "#1a3048" } else { "#24506e" }),
            padding: Edges::all(60.0),
            gap: 40.0,
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
                        EventAction::Set(APP_SIGNAL, *id),
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
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#0f0f1a"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar("Files", "#3498db"),
            Component::row(
                Style {
                    flex_grow: 1.0,
                    gap: 0.0,
                    ..Style::default()
                },
                vec![
                    // Sidebar
                    Component::column(
                        Style {
                            width: Dimension::Px(200.0),
                            background: Color::from_hex("#10101c"),
                            padding: Edges::all(12.0),
                            gap: 4.0,
                            ..Style::default()
                        },
                        vec![
                            sidebar_item("Home", true),
                            sidebar_item("Documents", false),
                            sidebar_item("Pictures", false),
                            sidebar_item("Downloads", false),
                            sidebar_item("Music", false),
                        ],
                    ),
                    // File list
                    Component::column(
                        Style {
                            flex_grow: 1.0,
                            padding: Edges::all(16.0),
                            gap: 4.0,
                            ..Style::default()
                        },
                        vec![
                            file_row("projects", "Folder", "--", true),
                            file_row("w3cos", "Folder", "--", true),
                            file_row("README.md", "Markdown", "4.2 KB", false),
                            file_row("Cargo.toml", "TOML", "1.1 KB", false),
                            file_row("app.tsx", "TypeScript", "2.8 KB", false),
                            file_row("screenshot.png", "PNG Image", "245 KB", false),
                            Component::text(
                                "6 items, 253 KB",
                                Style {
                                    font_size: 11.0,
                                    color: Color::from_hex("#606080"),
                                    padding: Edges::all(8.0),
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

fn sidebar_item(name: &str, active: bool) -> Component {
    let bg = if active { "#3d5570" } else { "#243447" };
    let color = if active { "#f4f7fb" } else { "#d5dee8" };
    Component::text(
        name,
        Style {
            font_size: 13.0,
            color: Color::from_hex(color),
            background: Color::from_hex(bg),
            border_radius: 6.0,
            padding: Edges::xy(10.0, 8.0),
            ..Style::default()
        },
    )
}

fn file_row(name: &str, kind: &str, size: &str, is_dir: bool) -> Component {
    let icon = if is_dir { "[DIR]" } else { "    " };
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
            Component::text(
                name,
                Style {
                    font_size: 13.0,
                    color: Color::from_hex("#d0d0e0"),
                    flex_grow: 1.0,
                    ..Style::default()
                },
            ),
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
            app_titlebar("终端 Terminal", "#1c8f62"),
            Component::column(
                Style {
                    flex_grow: 1.0,
                    padding: Edges::all(16.0),
                    gap: 10.0,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        "本机终端占位。命令还不会执行。",
                        Style {
                            font_size: 16.0,
                            font_weight: 700,
                            color: Color::from_hex("#f4f7fb"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "Local terminal placeholder. Typed commands stay on this page.",
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
                                "",
                                "help",
                                Style {
                                    font_size: 14.0,
                                    color: Color::from_hex("#f4f7fb"),
                                    background: Color::from_hex("#1b2c3e"),
                                    flex_grow: 1.0,
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

// ==========================================================================
// Settings App
// ==========================================================================

fn build_settings_app() -> Component {
    let night = if state::get_signal(NIGHT_SIGNAL) != 0 {
        "开 On"
    } else {
        "关 Off"
    };
    let storage_owned = state::get_signal_text(STORAGE_SIGNAL);
    let storage_label = if storage_owned.is_empty() {
        "memory".to_string()
    } else {
        storage_owned
    };
    let bridge = std::env::var("W3COS_AI_PERMISSIONS").unwrap_or_default();
    let bridge = match bridge.trim() {
        "" | "observer" => "observer",
        "interactive" => "interactive",
        "system" => "system",
        _ => "observer",
    };
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#24384a"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            app_titlebar("设置 Settings", "#5b4fe0"),
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
                            sidebar_item("本机 Local", true),
                            sidebar_item("身份 Identity", false),
                            sidebar_item("桥接 Bridge", false),
                        ],
                    ),
                    Component::column(
                        Style {
                            flex_grow: 1.0,
                            padding: Edges::all(20.0),
                            gap: 12.0,
                            ..Style::default()
                        },
                        vec![
                            Component::text(
                                "本机设置 · Local preferences",
                                Style {
                                    font_size: 18.0,
                                    color: Color::from_hex("#f4f7fb"),
                                    font_weight: 700,
                                    ..Style::default()
                                },
                            ),
                            Component::text(
                                "没有账号，也没有链。Passkey / did:key 仍是下一步。",
                                Style {
                                    font_size: 13.0,
                                    color: Color::from_hex("#d5dee8"),
                                    ..Style::default()
                                },
                            ),
                            settings_row("身份 Identity", "PBKDF2 · not a passkey"),
                            settings_row("夜间模式 Night", night),
                            settings_row("AI 权限 Bridge", bridge),
                            settings_row("收据 Receipts", &storage_label),
                        ],
                    ),
                ],
            ),
        ],
    )
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
    } else if phase == 2 {
        let notice = state::get_signal_text(NOTICE_SIGNAL);
        if !notice.is_empty() {
            rows.push(result_card(&notice));
        }
    }
    Component::column(
        Style {
            padding: Edges::xy(16.0, 10.0),
            gap: 8.0,
            background: Color::from_hex("#1e3144"),
            ..Style::default()
        },
        rows,
    )
}

fn confirm_card() -> Component {
    Component::column(
        Style {
            gap: 6.0,
            padding: Edges::all(12.0),
            background: Color::from_hex("#31475e"),
            border_radius: 8.0,
            ..Style::default()
        },
        vec![
            Component::text(
                "确认后才会执行 · Confirm to run",
                Style {
                    font_size: 12.0,
                    font_weight: 700,
                    color: Color::from_hex("#ffe08a"),
                    ..Style::default()
                },
            ),
            Component::text(
                state::get_signal_text(SUMMARY_SIGNAL),
                Style {
                    font_size: 16.0,
                    font_weight: 700,
                    color: ink(),
                    ..Style::default()
                },
            ),
            Component::text(
                state::get_signal_text(IMPACT_SIGNAL),
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
                    chrome_button("确认 Confirm", OP_CONFIRM, "#1c8f62"),
                    chrome_button("取消 Cancel", OP_CANCEL, "#3d5570"),
                ],
            ),
        ],
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
            if state::get_signal_text(STORAGE_SIGNAL) == "encrypted-file" {
                "L0 无模型 · 加密文件 Encrypted file"
            } else {
                "L0 无模型 · 仅内存 Memory"
            },
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
            format!(
                "{} · {} · 口令经 KDF 保存，还不是 passkey",
                LocalIdentityStub::SUBJECT,
                if unlocked {
                    "stub unlocked"
                } else {
                    "locked"
                }
            ),
            Style {
                font_size: 12.0,
                color: muted_ink(),
                ..Style::default()
            },
        ),
        chrome_button(
            if unlocked {
                "锁定 Lock"
            } else {
                "解锁 Unlock"
            },
            if unlocked { OP_LOCK } else { OP_UNLOCK },
            "#3d5570",
        ),
        Component::text(
            "收据 Receipts",
            Style {
                font_size: 14.0,
                font_weight: 700,
                color: ink(),
                ..Style::default()
            },
        ),
    ];
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
    if phase == 1 {
        body.push(chrome_button("停止 Stop", OP_CANCEL, "#a33b45"));
    }
    Component::column(
        Style {
            width: Dimension::Px(300.0),
            padding: Edges::all(14.0),
            gap: 8.0,
            background: Color::from_hex("#1b2c3e"),
            ..Style::default()
        },
        body,
    )
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
            app_titlebar("助手 Agent", "#e07a5f"),
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
            app_titlebar("Browser", "#f39c12"),
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
            app_titlebar("Editor", "#1abc9c"),
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

fn taskbar_icon(letter: &str, color: &str, id: i64) -> Component {
    Component::button_with_click(
        letter,
        Style {
            width: Dimension::Px(40.0),
            height: Dimension::Px(40.0),
            background: Color::from_hex(color),
            border_radius: 8.0,
            font_size: 16.0,
            font_weight: 700,
            color: Color::WHITE,
            ..Style::default()
        },
        EventAction::Set(APP_SIGNAL, id),
    )
}

fn app_titlebar(title: &str, accent: &str) -> Component {
    Component::row(
        Style {
            height: Dimension::Px(40.0),
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
                    Component::button(
                        "--",
                        Style {
                            font_size: 12.0,
                            color: Color::from_hex("#808090"),
                            background: Color::from_hex("#2a2a3e"),
                            border_radius: 4.0,
                            ..Style::default()
                        },
                    ),
                    Component::button(
                        "[]",
                        Style {
                            font_size: 12.0,
                            color: Color::from_hex("#808090"),
                            background: Color::from_hex("#2a2a3e"),
                            border_radius: 4.0,
                            ..Style::default()
                        },
                    ),
                    Component::button(
                        "X",
                        Style {
                            font_size: 12.0,
                            color: Color::from_hex("#e94560"),
                            background: Color::from_hex("#2a2a3e"),
                            border_radius: 4.0,
                            ..Style::default()
                        },
                    ),
                ],
            ),
        ],
    )
}

fn build_taskbar(current: i64) -> Component {
    let center_text = match current {
        0 => "W3C OS Desktop",
        1 => "Files",
        2 => "Terminal",
        3 => "Settings",
        4 => "AI Agent",
        5 => "Browser",
        6 => "Editor",
        _ => "W3C OS",
    };
    Component::row(
        Style {
            height: Dimension::Px(56.0),
            background: Color::from_hex("#162433"),
            padding: Edges::xy(16.0, 8.0),
            gap: 12.0,
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
                        "W3C Apps",
                        Style {
                            background: Color::from_hex("#6c5ce7"),
                            border_radius: 8.0,
                            font_size: 14.0,
                            font_weight: 600,
                            color: Color::WHITE,
                            padding: Edges::xy(14.0, 8.0),
                            ..Style::default()
                        },
                        EventAction::Set(APP_SIGNAL, 0),
                    ),
                    taskbar_icon("F", "#3498db", 1),
                    taskbar_icon("T", "#2ecc71", 2),
                    taskbar_icon("S", "#9b59b6", 3),
                    taskbar_icon("A", "#e74c3c", 4),
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
                    gap: 16.0,
                    align_items: AlignItems::Center,
                    ..Style::default()
                },
                vec![
                    Component::text(
                        "Online",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#00b894"),
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "92%",
                        Style {
                            font_size: 13.0,
                            color: Color::from_hex("#a0a0c0"),
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
