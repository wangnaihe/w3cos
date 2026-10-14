//! Typed capability boundary for one Browser page.
//!
//! The page domain owns its `DocumentLoader`, HTML parser, image decoders,
//! JavaScript Realm and thread-local DOM on a dedicated worker. The embedding
//! shell receives immutable render snapshots and navigation/download results;
//! it cannot reach live page objects or grant filesystem/device capabilities.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::browser_controller::{BrowserChromeState, BrowserController, BrowserMode, Download};
use crate::dynamic_script::{DocumentLoadProgress, DocumentLoaderOptions};

#[derive(Debug)]
pub enum PageCommand {
    Navigate(String),
    Reload,
    Back,
    Forward,
    Stop,
    Download {
        url: String,
        suggested_name: Option<String>,
    },
    /// Parse a local document on this worker. There is no grant command:
    /// the worker's [`crate::capability_gate`] starts empty and stays empty.
    LoadDocument {
        html: String,
        url: String,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum PageEvent {
    Chrome(BrowserChromeState),
    Progress(DocumentLoadProgress),
    Snapshot(w3cos_std::Component),
    Traversal {
        direction: &'static str,
        moved: bool,
    },
    Download(Result<Download, String>),
    Error(String),
    Stopped,
    /// Reported after the worker clears its own permission store.
    Gate {
        permission: &'static str,
        granted: bool,
    },
}

pub struct BrowserPageDomain {
    commands: Sender<PageCommand>,
    events: Receiver<PageEvent>,
    worker: Option<JoinHandle<()>>,
}

impl BrowserPageDomain {
    pub fn spawn(mode: BrowserMode, options: DocumentLoaderOptions) -> std::io::Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("w3cos-browser-page".to_string())
            .spawn(move || page_worker(mode, options, command_rx, event_tx))?;
        Ok(Self {
            commands: command_tx,
            events: event_rx,
            worker: Some(worker),
        })
    }

    pub fn send(&self, command: PageCommand) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "Browser page domain has stopped".to_string())
    }

    pub fn try_recv(&self) -> Option<PageEvent> {
        self.events.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<PageEvent> {
        self.events.recv_timeout(timeout).ok()
    }
}

impl Drop for BrowserPageDomain {
    fn drop(&mut self) {
        let _ = self.commands.send(PageCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn page_worker(
    mode: BrowserMode,
    options: DocumentLoaderOptions,
    commands: Receiver<PageCommand>,
    events: Sender<PageEvent>,
) {
    // Permissions are thread-local. Resetting here drops anything the
    // embedding thread granted, including AI-bridge capability decisions.
    crate::capability_gate::reset();
    let mut controller = BrowserController::new(mode, options);
    let mut active = false;
    let mut last_progress = DocumentLoadProgress::Idle;
    let _ = events.send(PageEvent::Chrome(controller.chrome_state()));
    let _ = events.send(PageEvent::Gate {
        permission: "file-system",
        granted: crate::capability_gate::is_granted("file-system"),
    });
    loop {
        let command = if active {
            commands.recv_timeout(Duration::from_millis(8)).ok()
        } else {
            commands.recv().ok()
        };
        if let Some(command) = command {
            let result = match command {
                PageCommand::Navigate(url) => {
                    active = true;
                    controller.navigate(&url).map(|_| None)
                }
                PageCommand::Reload => {
                    active = true;
                    controller.reload().map(|_| None)
                }
                PageCommand::Back => {
                    active = true;
                    controller.back().map(|moved| {
                        Some(PageEvent::Traversal {
                            direction: "back",
                            moved,
                        })
                    })
                }
                PageCommand::Forward => {
                    active = true;
                    controller.forward().map(|moved| {
                        Some(PageEvent::Traversal {
                            direction: "forward",
                            moved,
                        })
                    })
                }
                PageCommand::Stop => {
                    controller.stop();
                    active = false;
                    let _ = events.send(PageEvent::Stopped);
                    Ok(None)
                }
                PageCommand::Download {
                    url,
                    suggested_name,
                } => controller
                    .start_download(&url, suggested_name.as_deref())
                    .map(|_| None),
                PageCommand::LoadDocument { html, url } => {
                    crate::capability_gate::reset();
                    match crate::dynamic_script::load_html_on_shared_vm(&html, &url) {
                        Ok(()) => {
                            let snapshot = crate::dom::to_component_tree();
                            crate::dom::clear_document_dirty();
                            let _ = events.send(PageEvent::Snapshot(snapshot));
                            let _ =
                                events.send(PageEvent::Progress(DocumentLoadProgress::Complete));
                            active = false;
                            Ok(None)
                        }
                        Err(error) => Err(error),
                    }
                }
                PageCommand::Shutdown => break,
            };
            match result {
                Ok(Some(event)) => {
                    let _ = events.send(event);
                }
                Ok(None) => {}
                Err(error) => {
                    active = false;
                    let _ = events.send(PageEvent::Error(error.to_string()));
                }
            }
            let _ = events.send(PageEvent::Chrome(controller.chrome_state()));
        }

        if active {
            let progress = controller.poll();
            if progress != last_progress {
                last_progress = progress.clone();
                let _ = events.send(PageEvent::Progress(progress.clone()));
            }
            if crate::dom::is_document_dirty() {
                let snapshot = crate::dom::to_component_tree();
                crate::dom::clear_document_dirty();
                let _ = events.send(PageEvent::Snapshot(snapshot));
            }
            if matches!(
                progress,
                DocumentLoadProgress::Complete
                    | DocumentLoadProgress::Failed(_)
                    | DocumentLoadProgress::Cancelled
            ) {
                active = false;
                let _ = events.send(PageEvent::Chrome(controller.chrome_state()));
            }
        }
        if let Some(download) = controller.poll_download() {
            let _ = events.send(PageEvent::Download(download));
        }
    }
}

pub fn component_text(component: &w3cos_std::Component) -> String {
    let mut out = String::new();
    walk_text(component, &mut out);
    out
}

fn walk_text(component: &w3cos_std::Component, out: &mut String) {
    match &component.kind {
        w3cos_std::ComponentKind::Text { content } => {
            out.push_str(content);
            out.push('\n');
        }
        w3cos_std::ComponentKind::Button { label } => {
            out.push_str(label);
            out.push('\n');
        }
        _ => {}
    }
    for child in &component.children {
        walk_text(child, out);
    }
}

/// Run `html` on a `BrowserPageDomain` worker and return the snapshot the
/// worker produced. The caller never touches the page's DOM or VM.
pub fn load_html_snapshot(html: &str, url: &str) -> anyhow::Result<w3cos_std::Component> {
    let domain =
        BrowserPageDomain::spawn(BrowserMode::Interactive, DocumentLoaderOptions::default())
            .map_err(|error| anyhow::anyhow!("could not start the page worker: {error}"))?;
    domain
        .send(PageCommand::LoadDocument {
            html: html.to_string(),
            url: url.to_string(),
        })
        .map_err(|error| anyhow::anyhow!(error))?;
    let mut snapshot = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match domain.recv_timeout(Duration::from_millis(200)) {
            Some(PageEvent::Snapshot(component)) => snapshot = Some(component),
            Some(PageEvent::Progress(DocumentLoadProgress::Complete)) => break,
            Some(PageEvent::Progress(DocumentLoadProgress::Failed(message))) => {
                anyhow::bail!("page worker failed: {message}");
            }
            Some(PageEvent::Error(message)) => anyhow::bail!("page worker failed: {message}"),
            None => break,
            Some(_) => {}
        }
    }
    let mut snapshot =
        snapshot.ok_or_else(|| anyhow::anyhow!("page worker produced no snapshot"))?;
    // The worker keeps the live document. The snapshot the UI paints must not
    // carry host ids, or the UI thread would look those ids up in its own DOM.
    detach_host_ids(&mut snapshot);
    Ok(snapshot)
}

fn detach_host_ids(component: &mut w3cos_std::Component) {
    if matches!(
        component.on_click,
        w3cos_std::EventAction::NativeHost { .. }
    ) {
        component.on_click = w3cos_std::EventAction::None;
    }
    for child in &mut component.children {
        detach_host_ids(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_domain_exposes_only_typed_commands_and_snapshots() {
        let domain =
            BrowserPageDomain::spawn(BrowserMode::Reader, DocumentLoaderOptions::default())
                .unwrap();
        let event = domain.recv_timeout(Duration::from_secs(1));
        assert!(matches!(
            event,
            Some(PageEvent::Chrome(BrowserChromeState {
                reader_mode: true,
                ..
            }))
        ));
    }

    #[test]
    fn page_worker_does_not_inherit_parent_capability_grants() {
        crate::capability_gate::reset();
        crate::capability_gate::grant("file-system");
        let domain =
            BrowserPageDomain::spawn(BrowserMode::Reader, DocumentLoaderOptions::default())
                .unwrap();
        let mut isolated = false;
        for _ in 0..6 {
            match domain.recv_timeout(Duration::from_secs(2)) {
                Some(PageEvent::Gate {
                    permission,
                    granted,
                }) => {
                    assert_eq!(permission, "file-system");
                    assert!(!granted);
                    isolated = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(isolated);
        assert!(crate::capability_gate::is_granted("file-system"));
        crate::capability_gate::reset();
    }

    #[test]
    fn page_worker_runs_inline_script_into_a_snapshot() {
        let html = r#"<!doctype html><h1 id="msg">page loading</h1>
            <script>document.getElementById("msg").textContent = "Hello from W3VM";</script>"#;
        let snapshot = load_html_snapshot(html, "https://shared.w3cos.local/page.html").unwrap();
        let text = component_text(&snapshot);
        assert!(text.contains("Hello from W3VM"), "{text}");
        assert!(!text.contains("page loading"), "{text}");
    }
}
