//! W3C OS Desktop Shell — the system-level GUI.
//!
//! This binary is what boots as the primary UI when W3C OS starts.
//! It provides: desktop, taskbar, app launcher, system tray, and window management.
//!
//! Boot sequence: Linux kernel → init → S99w3cos → w3cos-shell (this binary)

use w3cos_std::style::*;
use w3cos_std::color::Color;
use w3cos_std::{Component, EventAction, Style};

fn main() {
    eprintln!("╔═══════════════════════════════════════╗");
    eprintln!("║     W3C OS Desktop Shell v0.1.0       ║");
    eprintln!("║     Native GUI — No browser, No V8    ║");
    eprintln!("╚═══════════════════════════════════════╝");

    w3cos_runtime::run_app(build_shell).expect("W3C OS Shell crashed");
}

fn build_shell() -> Component {
    let active_app = w3cos_runtime::state::create_signal(0);
    let _ = w3cos_runtime::state::get_signal(active_app);

    Component::column(
        Style {
            background: Color::from_hex("#0a0a14"),
            gap: 0.0,
            ..Style::default()
        },
        vec![
            build_desktop(active_app),
            build_taskbar(active_app),
        ],
    )
}

fn build_desktop(active_signal: usize) -> Component {
    Component::column(
        Style {
            flex_grow: 1.0,
            background: Color::from_hex("#0f1923"),
            padding: Edges::all(40.0),
            gap: 32.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..Style::default()
        },
        vec![
            build_desktop_icons(active_signal),
            build_branding(),
        ],
    )
}

fn build_desktop_icons(active_signal: usize) -> Component {
    let apps = [
        ("📁", "Files", 1),
        ("⌨", "Terminal", 2),
        ("⚙", "Settings", 3),
        ("🤖", "AI Agent", 4),
        ("🌐", "Browser", 5),
        ("📝", "Editor", 6),
    ];

    let icons: Vec<Component> = apps
        .iter()
        .map(|(icon, label, id)| {
            Component::column(
                Style {
                    align_items: AlignItems::Center,
                    gap: 8.0,
                    ..Style::default()
                },
                vec![
                    Component::button_with_click(
                        *icon,
                        Style {
                            width: Dimension::Px(56.0),
                            height: Dimension::Px(56.0),
                            background: Color::from_hex("#1a1a2e"),
                            border_radius: 12.0,
                            font_size: 28.0,
                            ..Style::default()
                        },
                        EventAction::Set(active_signal, *id),
                    ),
                    Component::text(
                        *label,
                        Style {
                            font_size: 11.0,
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
            gap: 32.0,
            ..Style::default()
        },
        icons,
    )
}

fn build_branding() -> Component {
    Component::column(
        Style {
            align_items: AlignItems::Center,
            gap: 4.0,
            opacity: 0.4,
            ..Style::default()
        },
        vec![
            Component::text(
                "W3C OS",
                Style {
                    font_size: 16.0,
                    color: Color::from_hex("#6c5ce7"),
                    font_weight: 700,
                    ..Style::default()
                },
            ),
            Component::text(
                "Native Desktop Shell — TypeScript compiled to native binary",
                Style {
                    font_size: 11.0,
                    color: Color::from_hex("#808090"),
                    ..Style::default()
                },
            ),
        ],
    )
}

fn build_taskbar(active_signal: usize) -> Component {
    Component::row(
        Style {
            height: Dimension::Px(48.0),
            background: Color::from_hex("#12121f"),
            padding: Edges::all(8.0),
            gap: 8.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..Style::default()
        },
        vec![
            build_taskbar_left(active_signal),
            build_taskbar_center(),
            build_taskbar_right(),
        ],
    )
}

fn build_taskbar_left(active_signal: usize) -> Component {
    let pinned = [("📁", 1), ("⌨", 2), ("⚙", 3)];
    let mut items = vec![Component::button_with_click(
        "◆ Apps",
        Style {
            background: Color::from_hex("#6c5ce7"),
            border_radius: 8.0,
            font_size: 14.0,
            color: Color::WHITE,
            ..Style::default()
        },
        EventAction::Set(active_signal, 0),
    )];

    for (icon, id) in pinned {
        items.push(Component::button_with_click(
            icon,
            Style {
                width: Dimension::Px(36.0),
                height: Dimension::Px(36.0),
                background: Color::from_hex("#1c1c34"),
                border_radius: 8.0,
                font_size: 18.0,
                ..Style::default()
            },
            EventAction::Set(active_signal, id),
        ));
    }

    Component::row(
        Style {
            gap: 6.0,
            align_items: AlignItems::Center,
            ..Style::default()
        },
        items,
    )
}

fn build_taskbar_center() -> Component {
    Component::text(
        "W3C OS Desktop",
        Style {
            font_size: 13.0,
            color: Color::from_hex("#808090"),
            ..Style::default()
        },
    )
}

fn build_taskbar_right() -> Component {
    Component::row(
        Style {
            gap: 12.0,
            align_items: AlignItems::Center,
            ..Style::default()
        },
        vec![
            Component::text(
                "● Online",
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#00b894"),
                    ..Style::default()
                },
            ),
            Component::text(
                "🔋 92%",
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#a0a0c0"),
                    ..Style::default()
                },
            ),
            Component::text(
                "14:32",
                Style {
                    font_size: 12.0,
                    color: Color::from_hex("#d0d0e0"),
                    background: Color::from_hex("#1c1c34"),
                    border_radius: 6.0,
                    padding: Edges::all(6.0),
                    ..Style::default()
                },
            ),
        ],
    )
}
