//! W3C OS Desktop Shell — the system-level GUI.
//!
//! Boot sequence: Linux kernel → init → S99w3cos → w3cos-shell (this binary)
//! Click an app icon → the app UI replaces the desktop area.
//! Click "W3C Apps" in taskbar → return to desktop.

use w3cos_std::style::*;
use w3cos_std::color::Color;
use w3cos_std::{Component, EventAction, Style};

fn main() {
    eprintln!("W3C OS Desktop Shell v0.1.0 — starting...");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--ai-port" {
            if let Some(port_str) = args.next() {
                if let Ok(port) = port_str.parse::<u16>() {
                    w3cos_runtime::enable_ai_bridge(port);
                    eprintln!("[AI Bridge] Will start on port {port}");
                }
            }
        }
    }

    w3cos_runtime::run_app(build_shell).expect("W3C OS Shell crashed");
}

const APP_SIGNAL: usize = 0;

fn build_shell() -> Component {
    let _ = w3cos_runtime::state::create_signal(0);
    let current = w3cos_runtime::state::get_signal(APP_SIGNAL);

    let content = match current {
        0 => build_desktop(),
        1 => build_files_app(),
        2 => build_terminal_app(),
        3 => build_settings_app(),
        4 => build_ai_agent_app(),
        _ => build_desktop(),
    };

    Component::column(
        Style {
            width: Dimension::Vw(100.0),
            height: Dimension::Vh(100.0),
            background: Color::from_hex("#0a0a14"),
            gap: 0.0,
            ..Style::default()
        },
        vec![content, build_taskbar(current)],
    )
}

// ==========================================================================
// Desktop (home screen)
// ==========================================================================

fn build_desktop() -> Component {
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#0f1923"),
            padding: Edges::all(60.0),
            gap: 40.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..Style::default()
        },
        vec![build_desktop_icons(), build_system_info(), build_branding()],
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
    let icons: Vec<Component> = apps.iter().map(|(l, label, c, id)| {
        Component::column(Style { align_items: AlignItems::Center, gap: 12.0, ..Style::default() }, vec![
            Component::button_with_click(*l, Style {
                width: Dimension::Px(80.0), height: Dimension::Px(80.0),
                background: Color::from_hex(c), border_radius: 16.0,
                font_size: 32.0, font_weight: 700, color: Color::WHITE,
                ..Style::default()
            }, EventAction::Set(APP_SIGNAL, *id)),
            Component::text(*label, Style { font_size: 14.0, color: Color::from_hex("#c0c0d0"), ..Style::default() }),
        ])
    }).collect();
    Component::row(Style { gap: 40.0, flex_wrap: FlexWrap::Wrap, justify_content: JustifyContent::Center, ..Style::default() }, icons)
}

fn build_system_info() -> Component {
    Component::row(Style { gap: 24.0, justify_content: JustifyContent::Center, flex_wrap: FlexWrap::Wrap, ..Style::default() }, vec![
        info_card("CPU", "23%", "#00b894"),
        info_card("Memory", "5.4 / 8 GB", "#fdcb6e"),
        info_card("Storage", "205 / 512 GB", "#74b9ff"),
        info_card("Network", "84 Mbps", "#a29bfe"),
    ])
}

fn info_card(title: &str, value: &str, accent: &str) -> Component {
    Component::column(Style {
        padding: Edges::all(20.0), background: Color::from_hex("#16162a"),
        border_radius: 12.0, gap: 8.0, min_width: Dimension::Px(160.0), flex_grow: 1.0,
        ..Style::default()
    }, vec![
        Component::text(title, Style { font_size: 13.0, color: Color::from_hex("#808090"), ..Style::default() }),
        Component::text(value, Style { font_size: 22.0, color: Color::from_hex("#f0f0ff"), font_weight: 700, ..Style::default() }),
        Component::row(Style { height: Dimension::Px(6.0), border_radius: 3.0, background: Color::from_hex("#1e1e38"), ..Style::default() }, vec![
            Component::column(Style { width: Dimension::Percent(40.0), height: Dimension::Px(6.0), border_radius: 3.0, background: Color::from_hex(accent), ..Style::default() }, vec![]),
        ]),
    ])
}

fn build_branding() -> Component {
    Component::column(Style { align_items: AlignItems::Center, gap: 6.0, opacity: 0.5, ..Style::default() }, vec![
        Component::text("W3C OS", Style { font_size: 20.0, color: Color::from_hex("#6c5ce7"), font_weight: 700, ..Style::default() }),
        Component::text("Native Desktop Shell - TypeScript compiled to native binary", Style { font_size: 13.0, color: Color::from_hex("#808090"), ..Style::default() }),
    ])
}

// ==========================================================================
// Files App
// ==========================================================================

fn build_files_app() -> Component {
    Component::column(Style { flex_grow: 1.0, background: Color::from_hex("#0f0f1a"), gap: 0.0, ..Style::default() }, vec![
        app_titlebar("Files", "#3498db"),
        Component::row(Style { flex_grow: 1.0, gap: 0.0, ..Style::default() }, vec![
            // Sidebar
            Component::column(Style {
                width: Dimension::Px(200.0), background: Color::from_hex("#10101c"),
                padding: Edges::all(12.0), gap: 4.0,
                ..Style::default()
            }, vec![
                sidebar_item("Home", true),
                sidebar_item("Documents", false),
                sidebar_item("Pictures", false),
                sidebar_item("Downloads", false),
                sidebar_item("Music", false),
            ]),
            // File list
            Component::column(Style { flex_grow: 1.0, padding: Edges::all(16.0), gap: 4.0, ..Style::default() }, vec![
                file_row("projects", "Folder", "--", true),
                file_row("w3cos", "Folder", "--", true),
                file_row("README.md", "Markdown", "4.2 KB", false),
                file_row("Cargo.toml", "TOML", "1.1 KB", false),
                file_row("app.tsx", "TypeScript", "2.8 KB", false),
                file_row("screenshot.png", "PNG Image", "245 KB", false),
                Component::text("6 items, 253 KB", Style { font_size: 11.0, color: Color::from_hex("#606080"), padding: Edges::all(8.0), ..Style::default() }),
            ]),
        ]),
    ])
}

fn sidebar_item(name: &str, active: bool) -> Component {
    let bg = if active { "#6c5ce7" } else { "#10101c" };
    let color = if active { "#ffffff" } else { "#a0a0c0" };
    Component::text(name, Style {
        font_size: 13.0, color: Color::from_hex(color),
        background: Color::from_hex(bg), border_radius: 6.0,
        padding: Edges::xy(10.0, 8.0),
        ..Style::default()
    })
}

fn file_row(name: &str, kind: &str, size: &str, is_dir: bool) -> Component {
    let icon = if is_dir { "[DIR]" } else { "    " };
    Component::row(Style {
        padding: Edges::xy(8.0, 10.0), gap: 16.0, border_radius: 4.0,
        align_items: AlignItems::Center,
        ..Style::default()
    }, vec![
        Component::text(icon, Style { font_size: 12.0, color: Color::from_hex("#6c5ce7"), ..Style::default() }),
        Component::text(name, Style { font_size: 13.0, color: Color::from_hex("#d0d0e0"), flex_grow: 1.0, ..Style::default() }),
        Component::text(size, Style { font_size: 12.0, color: Color::from_hex("#808090"), ..Style::default() }),
        Component::text(kind, Style { font_size: 12.0, color: Color::from_hex("#808090"), ..Style::default() }),
    ])
}

// ==========================================================================
// Terminal App
// ==========================================================================

fn build_terminal_app() -> Component {
    Component::column(Style { flex_grow: 1.0, background: Color::from_hex("#0c0c14"), gap: 0.0, ..Style::default() }, vec![
        app_titlebar("Terminal", "#2ecc71"),
        Component::column(Style {
            flex_grow: 1.0, padding: Edges::all(16.0), gap: 2.0,
            overflow: Overflow::Scroll,
            ..Style::default()
        }, vec![
            Component::text("W3C OS Terminal v0.1.0", Style { font_size: 14.0, color: Color::from_hex("#00b894"), ..Style::default() }),
            Component::text("Type 'help' for available commands.", Style { font_size: 13.0, color: Color::from_hex("#606080"), ..Style::default() }),
            Component::text("", Style { font_size: 13.0, ..Style::default() }),
            term_prompt("cargo build --release"),
            Component::text("   Compiling w3cos-std v0.1.0", Style { font_size: 13.0, color: Color::from_hex("#a0a0c0"), ..Style::default() }),
            Component::text("   Compiling w3cos-dom v0.1.0", Style { font_size: 13.0, color: Color::from_hex("#a0a0c0"), ..Style::default() }),
            Component::text("   Compiling w3cos-runtime v0.1.0", Style { font_size: 13.0, color: Color::from_hex("#a0a0c0"), ..Style::default() }),
            Component::text("   Compiling w3cos-shell v0.1.0", Style { font_size: 13.0, color: Color::from_hex("#a0a0c0"), ..Style::default() }),
            Component::text("    Finished release [optimized] in 42.3s", Style { font_size: 13.0, color: Color::from_hex("#00b894"), ..Style::default() }),
            Component::text("", Style { font_size: 13.0, ..Style::default() }),
            term_prompt("./target/release/w3cos-shell"),
            Component::text("W3C OS Desktop Shell v0.1.0 -- starting...", Style { font_size: 13.0, color: Color::from_hex("#a0a0c0"), ..Style::default() }),
            Component::text("", Style { font_size: 13.0, ..Style::default() }),
            Component::row(Style { gap: 0.0, ..Style::default() }, vec![
                Component::text("user@w3cos:~$ ", Style { font_size: 13.0, color: Color::from_hex("#6c5ce7"), ..Style::default() }),
                Component::text_input("", "", Style { font_size: 13.0, color: Color::from_hex("#d0d0e0"), background: Color::from_hex("#0c0c14"), flex_grow: 1.0, ..Style::default() }),
            ]),
        ]),
    ])
}

fn term_prompt(cmd: &str) -> Component {
    Component::row(Style { gap: 0.0, ..Style::default() }, vec![
        Component::text("user@w3cos:~$ ", Style { font_size: 13.0, color: Color::from_hex("#6c5ce7"), ..Style::default() }),
        Component::text(cmd, Style { font_size: 13.0, color: Color::from_hex("#d0d0e0"), ..Style::default() }),
    ])
}

// ==========================================================================
// Settings App
// ==========================================================================

fn build_settings_app() -> Component {
    Component::column(Style { flex_grow: 1.0, background: Color::from_hex("#0f0f1a"), gap: 0.0, ..Style::default() }, vec![
        app_titlebar("Settings", "#9b59b6"),
        Component::row(Style { flex_grow: 1.0, gap: 0.0, ..Style::default() }, vec![
            // Settings sidebar
            Component::column(Style {
                width: Dimension::Px(200.0), background: Color::from_hex("#10101c"),
                padding: Edges::all(12.0), gap: 4.0, ..Style::default()
            }, vec![
                sidebar_item("Display", true),
                sidebar_item("Network", false),
                sidebar_item("Sound", false),
                sidebar_item("AI Agents", false),
                sidebar_item("About", false),
            ]),
            // Settings content
            Component::column(Style { flex_grow: 1.0, padding: Edges::all(24.0), gap: 20.0, ..Style::default() }, vec![
                Component::text("Display Settings", Style { font_size: 20.0, color: Color::from_hex("#f0f0ff"), font_weight: 700, ..Style::default() }),
                settings_row("Theme", "Dark"),
                settings_row("Font Size", "14px"),
                settings_row("Resolution", "1920 x 1080"),
                settings_row("Scale", "100%"),
                settings_row("Refresh Rate", "60 Hz"),
                settings_toggle("Enable Animations", true),
                settings_toggle("Reduce Motion", false),
            ]),
        ]),
    ])
}

fn settings_row(label: &str, value: &str) -> Component {
    Component::row(Style {
        padding: Edges::all(14.0), background: Color::from_hex("#16162a"),
        border_radius: 8.0, justify_content: JustifyContent::SpaceBetween,
        align_items: AlignItems::Center,
        ..Style::default()
    }, vec![
        Component::text(label, Style { font_size: 14.0, color: Color::from_hex("#d0d0e0"), ..Style::default() }),
        Component::text(value, Style { font_size: 14.0, color: Color::from_hex("#6c5ce7"), ..Style::default() }),
    ])
}

fn settings_toggle(label: &str, on: bool) -> Component {
    let indicator = if on { "[ON]" } else { "[OFF]" };
    let color = if on { "#00b894" } else { "#808090" };
    Component::row(Style {
        padding: Edges::all(14.0), background: Color::from_hex("#16162a"),
        border_radius: 8.0, justify_content: JustifyContent::SpaceBetween,
        align_items: AlignItems::Center,
        ..Style::default()
    }, vec![
        Component::text(label, Style { font_size: 14.0, color: Color::from_hex("#d0d0e0"), ..Style::default() }),
        Component::text(indicator, Style { font_size: 14.0, color: Color::from_hex(color), font_weight: 700, ..Style::default() }),
    ])
}

// ==========================================================================
// AI Agent App
// ==========================================================================

fn build_ai_agent_app() -> Component {
    Component::column(Style { flex_grow: 1.0, background: Color::from_hex("#0a0a12"), gap: 0.0, ..Style::default() }, vec![
        app_titlebar("AI Agent", "#e74c3c"),
        Component::row(Style { flex_grow: 1.0, gap: 0.0, ..Style::default() }, vec![
            // Agent sidebar
            Component::column(Style {
                width: Dimension::Px(220.0), background: Color::from_hex("#10101c"),
                padding: Edges::all(12.0), gap: 8.0, ..Style::default()
            }, vec![
                Component::text("ACTIVE AGENTS", Style { font_size: 11.0, color: Color::from_hex("#505070"), font_weight: 700, ..Style::default() }),
                agent_card("Code Agent", "Running", "#00b894", "Writing filesystem module..."),
                agent_card("Review Agent", "Waiting", "#fdcb6e", "Queued: PR #42"),
                agent_card("Test Agent", "Running", "#00b894", "47/52 tests passed"),
            ]),
            // Chat area
            Component::column(Style { flex_grow: 1.0, padding: Edges::all(20.0), gap: 16.0, ..Style::default() }, vec![
                Component::text("Agent Conversation", Style { font_size: 18.0, color: Color::from_hex("#f0f0ff"), font_weight: 700, ..Style::default() }),
                chat_msg("System", "Agent connected. DOM access granted (Layer 1 + 2). Permission: interactive.", "#141428", "#606080"),
                chat_msg("Code Agent", "I can see 47 DOM elements, 12 interactive buttons, 3 text inputs. All elements properly labeled in the a11y tree.", "#1c1c34", "#6c5ce7"),
                chat_msg("You", "Build a file manager with tree view sidebar.", "#6c5ce7", "#ffffff"),
                chat_msg("Code Agent", "Creating component tree via DOM API... 24 operations completed in 0.3ms.", "#1c1c34", "#6c5ce7"),
                Component::row(Style { gap: 8.0, align_items: AlignItems::Center, ..Style::default() }, vec![
                    Component::text_input("", "Ask the AI agent...", Style { flex_grow: 1.0, font_size: 14.0, color: Color::from_hex("#d0d0e0"), background: Color::from_hex("#1c1c34"), border_radius: 8.0, ..Style::default() }),
                    Component::button("Send", Style { background: Color::from_hex("#6c5ce7"), border_radius: 8.0, font_size: 14.0, color: Color::WHITE, ..Style::default() }),
                ]),
            ]),
        ]),
    ])
}

fn agent_card(name: &str, status: &str, status_color: &str, desc: &str) -> Component {
    Component::column(Style {
        padding: Edges::all(12.0), background: Color::from_hex("#1c1c34"),
        border_radius: 8.0, gap: 6.0, ..Style::default()
    }, vec![
        Component::row(Style { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..Style::default() }, vec![
            Component::text(name, Style { font_size: 13.0, color: Color::from_hex("#d0d0e0"), font_weight: 600, ..Style::default() }),
            Component::text(status, Style { font_size: 11.0, color: Color::from_hex(status_color), ..Style::default() }),
        ]),
        Component::text(desc, Style { font_size: 11.0, color: Color::from_hex("#606080"), ..Style::default() }),
    ])
}

fn chat_msg(sender: &str, text: &str, bg: &str, sender_color: &str) -> Component {
    Component::column(Style {
        padding: Edges::all(12.0), background: Color::from_hex(bg),
        border_radius: 8.0, gap: 4.0, ..Style::default()
    }, vec![
        Component::text(sender, Style { font_size: 11.0, color: Color::from_hex(sender_color), font_weight: 600, ..Style::default() }),
        Component::text(text, Style { font_size: 13.0, color: Color::from_hex("#d0d0e0"), ..Style::default() }),
    ])
}

// ==========================================================================
// Shared: title bar + taskbar
// ==========================================================================

fn taskbar_icon(letter: &str, color: &str, id: i64) -> Component {
    Component::button_with_click(letter, Style {
        width: Dimension::Px(40.0), height: Dimension::Px(40.0),
        background: Color::from_hex(color), border_radius: 8.0,
        font_size: 16.0, font_weight: 700, color: Color::WHITE,
        ..Style::default()
    }, EventAction::Set(APP_SIGNAL, id))
}

fn app_titlebar(title: &str, accent: &str) -> Component {
    Component::row(Style {
        height: Dimension::Px(40.0), background: Color::from_hex("#1a1a2e"),
        padding: Edges::xy(16.0, 8.0), align_items: AlignItems::Center,
        justify_content: JustifyContent::SpaceBetween,
        ..Style::default()
    }, vec![
        Component::row(Style { gap: 10.0, align_items: AlignItems::Center, ..Style::default() }, vec![
            Component::column(Style { width: Dimension::Px(12.0), height: Dimension::Px(12.0), background: Color::from_hex(accent), border_radius: 6.0, ..Style::default() }, vec![]),
            Component::text(title, Style { font_size: 14.0, color: Color::from_hex("#e0e0f0"), font_weight: 600, ..Style::default() }),
        ]),
        Component::row(Style { gap: 6.0, ..Style::default() }, vec![
            Component::button("--", Style { font_size: 12.0, color: Color::from_hex("#808090"), background: Color::from_hex("#2a2a3e"), border_radius: 4.0, ..Style::default() }),
            Component::button("[]", Style { font_size: 12.0, color: Color::from_hex("#808090"), background: Color::from_hex("#2a2a3e"), border_radius: 4.0, ..Style::default() }),
            Component::button("X", Style { font_size: 12.0, color: Color::from_hex("#e94560"), background: Color::from_hex("#2a2a3e"), border_radius: 4.0, ..Style::default() }),
        ]),
    ])
}

fn build_taskbar( current: i64) -> Component {
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
    Component::row(Style {
        height: Dimension::Px(56.0), background: Color::from_hex("#12121f"),
        padding: Edges::xy(16.0, 8.0), gap: 12.0,
        align_items: AlignItems::Center, justify_content: JustifyContent::SpaceBetween,
        ..Style::default()
    }, vec![
        Component::row(Style { gap: 8.0, align_items: AlignItems::Center, ..Style::default() }, vec![
            Component::button_with_click("W3C Apps", Style {
                background: Color::from_hex("#6c5ce7"), border_radius: 8.0,
                font_size: 14.0, font_weight: 600, color: Color::WHITE,
                padding: Edges::xy(14.0, 8.0), ..Style::default()
            }, EventAction::Set(APP_SIGNAL, 0)),
            taskbar_icon("F", "#3498db", 1),
            taskbar_icon("T", "#2ecc71", 2),
            taskbar_icon("S", "#9b59b6", 3),
            taskbar_icon("A", "#e74c3c", 4),
        ]),
        Component::text(center_text, Style { font_size: 14.0, color: Color::from_hex("#808090"), ..Style::default() }),
        Component::row(Style { gap: 16.0, align_items: AlignItems::Center, ..Style::default() }, vec![
            Component::text("Online", Style { font_size: 13.0, color: Color::from_hex("#00b894"), ..Style::default() }),
            Component::text("92%", Style { font_size: 13.0, color: Color::from_hex("#a0a0c0"), ..Style::default() }),
            Component::text("14:32", Style { font_size: 14.0, color: Color::from_hex("#d0d0e0"), background: Color::from_hex("#1c1c34"), border_radius: 8.0, padding: Edges::xy(12.0, 6.0), ..Style::default() }),
        ]),
    ])
}
