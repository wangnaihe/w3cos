//! Viewport `interactive-widget` — maps to HTML `<meta name="viewport">` and Android `windowSoftInputMode`.
//!
//! See [CSS Viewport Module Level 1 — `interactive-widget`](https://drafts.csswg.org/css-viewport/#interactive-widget).

use std::sync::atomic::{AtomicU8, Ordering};

/// How the on-screen keyboard affects the layout / visual viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum InteractiveWidget {
    /// `interactive-widget=resizes-content` — layout viewport shrinks (Android `adjustResize`).
    #[default]
    ResizesContent = 0,
    /// `interactive-widget=resizes-visual` — visual viewport shrinks; native maps to content resize.
    ResizesVisual = 1,
    /// `interactive-widget=overlays-content` — keyboard overlays; use `env(keyboard-inset-height)` in CSS.
    OverlaysContent = 2,
}

impl InteractiveWidget {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "resizes-visual" => Self::ResizesVisual,
            "overlays-content" => Self::OverlaysContent,
            _ => Self::ResizesContent,
        }
    }

    pub fn as_meta_value(self) -> &'static str {
        match self {
            Self::ResizesContent => "resizes-content",
            Self::ResizesVisual => "resizes-visual",
            Self::OverlaysContent => "overlays-content",
        }
    }

    /// Whether the layout engine should shrink the root viewport when the IME is visible.
    pub fn resizes_layout_viewport(self) -> bool {
        matches!(self, Self::ResizesContent | Self::ResizesVisual)
    }
}

static INTERACTIVE_WIDGET: AtomicU8 = AtomicU8::new(InteractiveWidget::ResizesContent as u8);

pub fn set_interactive_widget(mode: InteractiveWidget) {
    INTERACTIVE_WIDGET.store(mode as u8, Ordering::Relaxed);
}

pub fn interactive_widget() -> InteractiveWidget {
    match INTERACTIVE_WIDGET.load(Ordering::Relaxed) {
        1 => InteractiveWidget::ResizesVisual,
        2 => InteractiveWidget::OverlaysContent,
        _ => InteractiveWidget::ResizesContent,
    }
}
