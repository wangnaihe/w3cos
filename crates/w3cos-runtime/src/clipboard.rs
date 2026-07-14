//! W3C Clipboard API — `navigator.clipboard`
//!
//! Mirrors the Clipboard API specification:
//! https://w3c.github.io/clipboard-apis/
//!
//! Provides synchronous and async variants. The sync variants are safe to
//! call from any thread; the async variants return an `mpsc::Receiver` that
//! resolves on a background thread (matching the Promise-based browser API).
//!
//! # Example
//! ```ignore
//! // Write to clipboard
//! navigator::clipboard::write_text("hello from w3cos").unwrap();
//!
//! // Read from clipboard
//! let text = navigator::clipboard::read_text().unwrap();
//! ```

use std::sync::mpsc;
use std::thread;

// ── ClipboardItem ──────────────────────────────────────────────────────────

/// W3C `ClipboardItem` — a single item on the clipboard with a MIME type.
#[derive(Debug, Clone)]
pub struct ClipboardItem {
    pub mime_type: String,
    pub data: ClipboardData,
}

#[derive(Debug, Clone)]
pub enum ClipboardData {
    Text(String),
    Bytes(Vec<u8>),
}

impl ClipboardItem {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            mime_type: "text/plain".into(),
            data: ClipboardData::Text(content.into()),
        }
    }

    pub fn html(content: impl Into<String>) -> Self {
        Self {
            mime_type: "text/html".into(),
            data: ClipboardData::Text(content.into()),
        }
    }

    pub fn bytes(mime_type: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            mime_type: mime_type.into(),
            data: ClipboardData::Bytes(data),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.data {
            ClipboardData::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

// ── Error ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ClipboardError(pub String);

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClipboardError: {}", self.0)
    }
}

impl std::error::Error for ClipboardError {}

// ── Clipboard (navigator.clipboard) ───────────────────────────────────────

/// W3C `Clipboard` interface — accessed via `navigator.clipboard`.
///
/// All methods have both a blocking (`_sync`) variant and an async variant
/// that returns an `mpsc::Receiver` (mirrors the browser's `Promise`).
pub struct Clipboard;

impl Clipboard {
    // ── writeText ──────────────────────────────────────────────────────────

    /// `navigator.clipboard.writeText(text)` — blocking.
    pub fn write_text(text: &str) -> Result<(), ClipboardError> {
        use arboard::Clipboard as Arboard;
        Arboard::new()
            .and_then(|mut cb| cb.set_text(text))
            .map_err(|e| ClipboardError(e.to_string()))
    }

    /// `navigator.clipboard.writeText(text)` — async (Promise equivalent).
    pub fn write_text_async(text: impl Into<String>) -> mpsc::Receiver<Result<(), ClipboardError>> {
        let (tx, rx) = mpsc::channel();
        let text = text.into();
        thread::spawn(move || {
            let _ = tx.send(Self::write_text(&text));
        });
        rx
    }

    // ── readText ───────────────────────────────────────────────────────────

    /// `navigator.clipboard.readText()` — blocking.
    pub fn read_text() -> Result<String, ClipboardError> {
        use arboard::Clipboard as Arboard;
        Arboard::new()
            .and_then(|mut cb| cb.get_text())
            .map_err(|e| ClipboardError(e.to_string()))
    }

    /// `navigator.clipboard.readText()` — async (Promise equivalent).
    pub fn read_text_async() -> mpsc::Receiver<Result<String, ClipboardError>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(Self::read_text());
        });
        rx
    }

    // ── write ──────────────────────────────────────────────────────────────

    /// `navigator.clipboard.write(items)` — write one or more `ClipboardItem`s.
    /// Currently supports `text/plain` and `text/html`; binary items are stored
    /// as base64-encoded text as a fallback.
    pub fn write(items: &[ClipboardItem]) -> Result<(), ClipboardError> {
        use arboard::Clipboard as Arboard;
        let mut cb = Arboard::new().map_err(|e| ClipboardError(e.to_string()))?;

        // Find text/plain first, then text/html, then fall back to first item
        let text_item = items
            .iter()
            .find(|i| i.mime_type == "text/plain")
            .or_else(|| items.iter().find(|i| i.mime_type == "text/html"))
            .or_else(|| items.first());

        if let Some(item) = text_item {
            match &item.data {
                ClipboardData::Text(s) => {
                    cb.set_text(s).map_err(|e| ClipboardError(e.to_string()))?;
                }
                ClipboardData::Bytes(b) => {
                    // Fallback: store as base64
                    use std::fmt::Write as FmtWrite;
                    let mut encoded = String::new();
                    for byte in b {
                        let _ = write!(encoded, "{:02x}", byte);
                    }
                    cb.set_text(&encoded)
                        .map_err(|e| ClipboardError(e.to_string()))?;
                }
            }
        }
        Ok(())
    }

    /// `navigator.clipboard.write(items)` — async variant.
    pub fn write_async(items: Vec<ClipboardItem>) -> mpsc::Receiver<Result<(), ClipboardError>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(Self::write(&items));
        });
        rx
    }

    // ── read ───────────────────────────────────────────────────────────────

    /// `navigator.clipboard.read()` — read clipboard contents as `ClipboardItem`s.
    pub fn read() -> Result<Vec<ClipboardItem>, ClipboardError> {
        let text = Self::read_text()?;
        Ok(vec![ClipboardItem::text(text)])
    }

    /// `navigator.clipboard.read()` — async variant.
    pub fn read_async() -> mpsc::Receiver<Result<Vec<ClipboardItem>, ClipboardError>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(Self::read());
        });
        rx
    }
}

// ── navigator module (mirrors browser global) ──────────────────────────────

/// `navigator.clipboard` — the global clipboard accessor.
pub mod navigator {
    pub mod clipboard {
        use super::super::{Clipboard, ClipboardError, ClipboardItem};
        use std::sync::mpsc;

        pub fn write_text(text: &str) -> Result<(), ClipboardError> {
            Clipboard::write_text(text)
        }
        pub fn write_text_async(
            text: impl Into<String>,
        ) -> mpsc::Receiver<Result<(), ClipboardError>> {
            Clipboard::write_text_async(text)
        }
        pub fn read_text() -> Result<String, ClipboardError> {
            Clipboard::read_text()
        }
        pub fn read_text_async() -> mpsc::Receiver<Result<String, ClipboardError>> {
            Clipboard::read_text_async()
        }
        pub fn write(items: &[ClipboardItem]) -> Result<(), ClipboardError> {
            Clipboard::write(items)
        }
        pub fn read() -> Result<Vec<ClipboardItem>, ClipboardError> {
            Clipboard::read()
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_item_text() {
        let item = ClipboardItem::text("hello");
        assert_eq!(item.mime_type, "text/plain");
        assert_eq!(item.as_text(), Some("hello"));
    }

    #[test]
    fn clipboard_item_html() {
        let item = ClipboardItem::html("<b>bold</b>");
        assert_eq!(item.mime_type, "text/html");
        assert_eq!(item.as_text(), Some("<b>bold</b>"));
    }

    // Note: write_text / read_text tests require a display server.
    // They are integration tests run with `cargo test --features integration`.
}
