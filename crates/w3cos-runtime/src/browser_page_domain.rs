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
    let mut controller = BrowserController::new(mode, options);
    let mut active = false;
    let mut last_progress = DocumentLoadProgress::Idle;
    let _ = events.send(PageEvent::Chrome(controller.chrome_state()));
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
}
