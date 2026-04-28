//! `w3cos.dialog` — native open / save / message dialogs.
//!
//! Powered by the `rfd` crate, which delegates to the platform-native dialog
//! provider (XDG Portal / GTK on Linux, Cocoa on macOS, Win32 on Windows).
//! Every call is forwarded to a dedicated worker thread so it never blocks
//! the main event loop, and the runtime hands callers a non-blocking
//! [`DialogReceiver`] that resolves into the user's selection (or `None` if
//! the dialog was dismissed).
//!
//! Mirrors a subset of the Electron `dialog` module:
//!
//! ```text
//! const file = await w3cos.dialog.showOpen({ filters: [{ name: "Text", extensions: ["txt"] }] });
//! const dir  = await w3cos.dialog.showOpenDirectory();
//! const dest = await w3cos.dialog.showSave({ defaultPath: "untitled.md" });
//! const ans  = await w3cos.dialog.showMessage({ title: "Quit?", buttons: ["Cancel", "Quit"] });
//! ```

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

/// File extension filter, matches Electron's `FileFilter` shape.
#[derive(Debug, Clone)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(name: impl Into<String>, extensions: &[&str]) -> Self {
        Self {
            name: name.into(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Options for [`show_open`] / [`show_open_multiple`] / [`show_open_directory`].
#[derive(Debug, Clone, Default)]
pub struct OpenDialogOptions {
    pub title: Option<String>,
    pub default_path: Option<PathBuf>,
    pub filters: Vec<FileFilter>,
}

/// Options for [`show_save`].
#[derive(Debug, Clone, Default)]
pub struct SaveDialogOptions {
    pub title: Option<String>,
    pub default_path: Option<PathBuf>,
    pub default_file_name: Option<String>,
    pub filters: Vec<FileFilter>,
}

/// Severity levels (mirrors Electron's `MessageBoxOptions.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    Info,
    Warning,
    Error,
    Question,
}

impl Default for MessageLevel {
    fn default() -> Self {
        MessageLevel::Info
    }
}

/// Options for [`show_message`].
#[derive(Debug, Clone)]
pub struct MessageDialogOptions {
    pub title: String,
    pub message: String,
    pub level: MessageLevel,
    /// Button labels in display order. The first matching label is the default.
    /// Empty == single OK button.
    pub buttons: Vec<String>,
}

impl Default for MessageDialogOptions {
    fn default() -> Self {
        Self {
            title: "W3C OS".into(),
            message: String::new(),
            level: MessageLevel::Info,
            buttons: Vec::new(),
        }
    }
}

/// Result of [`show_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageResult {
    /// User clicked the affirmative button (or the only OK button).
    Confirmed,
    /// User clicked a negative button.
    Cancelled,
    /// User picked a custom button — index into `buttons`.
    Custom(usize),
}

/// Async-style receiver. Drop the handle to ignore the result.
pub struct DialogReceiver<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> DialogReceiver<T> {
    /// Non-blocking poll. `None` while the user is still interacting.
    pub fn try_take(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }

    /// Block the calling thread until the user responds.
    /// Use this only outside the render loop (worker thread, CLI tool, etc.).
    pub fn wait(self) -> Option<T> {
        self.rx.recv().ok()
    }
}

fn spawn<T, F>(label: &'static str, work: F) -> DialogReceiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("w3cos-dialog-{label}"))
        .spawn(move || {
            let _ = tx.send(work());
        })
        .expect("spawn dialog worker");
    DialogReceiver { rx }
}

/// `w3cos.dialog.showOpen({filters})` → returns the chosen path (or `None`).
pub fn show_open(options: OpenDialogOptions) -> DialogReceiver<Option<PathBuf>> {
    spawn("open", move || {
        let mut dialog = rfd::FileDialog::new();
        dialog = apply_open_options(dialog, &options);
        dialog.pick_file()
    })
}

/// `w3cos.dialog.showOpen({multi: true})` → returns 0..N paths.
pub fn show_open_multiple(options: OpenDialogOptions) -> DialogReceiver<Vec<PathBuf>> {
    spawn("open-multi", move || {
        let mut dialog = rfd::FileDialog::new();
        dialog = apply_open_options(dialog, &options);
        dialog.pick_files().unwrap_or_default()
    })
}

/// `w3cos.dialog.showOpenDirectory()` → returns a directory path.
pub fn show_open_directory(options: OpenDialogOptions) -> DialogReceiver<Option<PathBuf>> {
    spawn("open-dir", move || {
        let mut dialog = rfd::FileDialog::new();
        if let Some(title) = options.title {
            dialog = dialog.set_title(&title);
        }
        if let Some(ref dir) = options.default_path {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_folder()
    })
}

/// `w3cos.dialog.showSave({defaultPath, filters})` → user-confirmed save path.
pub fn show_save(options: SaveDialogOptions) -> DialogReceiver<Option<PathBuf>> {
    spawn("save", move || {
        let mut dialog = rfd::FileDialog::new();
        if let Some(title) = options.title.as_deref() {
            dialog = dialog.set_title(title);
        }
        if let Some(ref dir) = options.default_path {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = options.default_file_name.as_deref() {
            dialog = dialog.set_file_name(name);
        }
        for filter in &options.filters {
            let exts: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter(&filter.name, &exts);
        }
        dialog.save_file()
    })
}

/// `w3cos.dialog.showMessage({...})` → which button the user pressed.
pub fn show_message(options: MessageDialogOptions) -> DialogReceiver<MessageResult> {
    spawn("message", move || {
        let mut dialog = rfd::MessageDialog::new()
            .set_title(&options.title)
            .set_description(&options.message);

        dialog = match options.level {
            MessageLevel::Info => dialog.set_level(rfd::MessageLevel::Info),
            MessageLevel::Warning => dialog.set_level(rfd::MessageLevel::Warning),
            MessageLevel::Error => dialog.set_level(rfd::MessageLevel::Error),
            MessageLevel::Question => dialog.set_level(rfd::MessageLevel::Info),
        };

        let buttons = match options.buttons.len() {
            0 => rfd::MessageButtons::Ok,
            1 => rfd::MessageButtons::OkCustom(options.buttons[0].clone()),
            2 => rfd::MessageButtons::OkCancelCustom(
                options.buttons[1].clone(),
                options.buttons[0].clone(),
            ),
            _ => rfd::MessageButtons::YesNoCancelCustom(
                options.buttons[0].clone(),
                options.buttons[1].clone(),
                options.buttons[2].clone(),
            ),
        };
        dialog = dialog.set_buttons(buttons);

        match dialog.show() {
            rfd::MessageDialogResult::Yes | rfd::MessageDialogResult::Ok => MessageResult::Confirmed,
            rfd::MessageDialogResult::No | rfd::MessageDialogResult::Cancel => MessageResult::Cancelled,
            rfd::MessageDialogResult::Custom(label) => options
                .buttons
                .iter()
                .position(|b| b == &label)
                .map(MessageResult::Custom)
                .unwrap_or(MessageResult::Confirmed),
        }
    })
}

fn apply_open_options(mut dialog: rfd::FileDialog, options: &OpenDialogOptions) -> rfd::FileDialog {
    if let Some(title) = options.title.as_deref() {
        dialog = dialog.set_title(title);
    }
    if let Some(ref dir) = options.default_path {
        dialog = dialog.set_directory(dir);
    }
    for filter in &options.filters {
        let exts: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(&filter.name, &exts);
    }
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_filter_construction() {
        let filter = FileFilter::new("Markdown", &["md", "markdown"]);
        assert_eq!(filter.name, "Markdown");
        assert_eq!(filter.extensions, vec!["md", "markdown"]);
    }

    #[test]
    fn dialog_receiver_try_take_initially_none() {
        let (tx, rx) = mpsc::channel::<Option<PathBuf>>();
        let receiver: DialogReceiver<Option<PathBuf>> = DialogReceiver { rx };
        assert!(receiver.try_take().is_none());
        tx.send(Some(PathBuf::from("/tmp/foo.txt"))).unwrap();
        assert_eq!(
            receiver.try_take(),
            Some(Some(PathBuf::from("/tmp/foo.txt")))
        );
    }

    #[test]
    fn message_options_defaults() {
        let opts = MessageDialogOptions::default();
        assert_eq!(opts.title, "W3C OS");
        assert_eq!(opts.level, MessageLevel::Info);
        assert!(opts.buttons.is_empty());
    }
}
