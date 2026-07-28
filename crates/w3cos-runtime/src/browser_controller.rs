//! UI-neutral Browser product controller.
//!
//! Platform shells bind address-bar and navigation controls to this type. It
//! reuses `DocumentLoader`, session history, the shared DOM and fetch transport
//! instead of introducing a second browser engine.

use std::sync::mpsc::TryRecvError;

use anyhow::{Result, anyhow};
use url::Url;

use crate::dynamic_script::{
    DocumentLoadProgress, DocumentLoader, DocumentLoaderOptions, ModuleCredentialsMode,
    ScriptPolicy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    Interactive,
    Reader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserChromeState {
    pub address: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
    pub reader_mode: bool,
}

#[derive(Debug)]
pub struct Download {
    pub final_url: String,
    pub suggested_name: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

struct PendingDownload {
    suggested_name: String,
    task: crate::fetch::BinaryFetchTask,
}

pub struct BrowserController {
    loader: DocumentLoader,
    mode: BrowserMode,
    pending_download: Option<PendingDownload>,
    reader_ui_installed: bool,
}

impl BrowserController {
    pub fn new(mode: BrowserMode, options: DocumentLoaderOptions) -> Self {
        let policy = ScriptPolicy {
            allow_scripts: mode == BrowserMode::Interactive,
            ..ScriptPolicy::default()
        };
        Self {
            loader: DocumentLoader::new(policy, options),
            mode,
            pending_download: None,
            reader_ui_installed: false,
        }
    }

    pub fn chrome_state(&self) -> BrowserChromeState {
        let address = self
            .loader
            .final_url()
            .or_else(|| self.loader.requested_url())
            .map(str::to_string)
            .unwrap_or_else(crate::history::get_href);
        BrowserChromeState {
            address,
            can_go_back: crate::history::can_go_back(),
            can_go_forward: crate::history::can_go_forward(),
            loading: !matches!(
                self.loader.progress(),
                DocumentLoadProgress::Idle
                    | DocumentLoadProgress::Complete
                    | DocumentLoadProgress::Failed(_)
                    | DocumentLoadProgress::Cancelled
            ),
            reader_mode: self.mode == BrowserMode::Reader,
        }
    }

    pub fn navigate(&mut self, address: &str) -> Result<()> {
        let target = normalize_address(address)?;
        if is_same_document_navigation(&crate::history::get_href(), target.as_str()) {
            crate::history::location_assign(target.as_str());
            scroll_to_fragment(&target);
            return Ok(());
        }
        crate::history::push_state(None, "", target.as_str());
        self.reader_ui_installed = false;
        self.loader.navigate(target.as_str())
    }

    pub fn reload(&mut self) -> Result<()> {
        let address = crate::history::get_href();
        self.reader_ui_installed = false;
        self.loader.navigate(&address)
    }

    pub fn back(&mut self) -> Result<bool> {
        if !crate::history::back() {
            return Ok(false);
        }
        self.reader_ui_installed = false;
        self.loader.navigate(&crate::history::get_href())?;
        Ok(true)
    }

    pub fn forward(&mut self) -> Result<bool> {
        if !crate::history::forward() {
            return Ok(false);
        }
        self.reader_ui_installed = false;
        self.loader.navigate(&crate::history::get_href())?;
        Ok(true)
    }

    pub fn poll(&mut self) -> DocumentLoadProgress {
        let progress = self.loader.poll();
        if progress == DocumentLoadProgress::Complete && self.mode == BrowserMode::Reader {
            self.install_reader_ui();
        }
        if progress == DocumentLoadProgress::Complete
            && let Ok(url) = Url::parse(&crate::history::get_href())
        {
            scroll_to_fragment(&url);
        }
        progress
    }

    pub fn stop(&mut self) {
        self.loader.cancel();
    }

    /// Begin a download but leave destination selection/writing to the shell.
    /// This prevents the page process from receiving arbitrary filesystem
    /// authority.
    pub fn start_download(&mut self, address: &str, suggested_name: Option<&str>) -> Result<()> {
        let url = normalize_address(address)?;
        let origin = url.origin().ascii_serialization();
        let name = suggested_name
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                url.path_segments()
                    .and_then(Iterator::last)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "download".to_string());
        if let Some(pending) = self.pending_download.take() {
            pending.task.cancel();
        }
        let task = crate::fetch::fetch_script_bytes_async(
            url.as_str(),
            crate::fetch::FetchOptions::default(),
            origin,
            crate::cookie_store_web::snapshot(),
            ModuleCredentialsMode::Include,
            false,
            crate::history::get_href(),
            crate::fetch::ScriptReferrerPolicy::default(),
        );
        self.pending_download = Some(PendingDownload {
            suggested_name: sanitize_download_name(&name),
            task,
        });
        Ok(())
    }

    pub fn poll_download(&mut self) -> Option<Result<Download, String>> {
        let result = match self.pending_download.as_ref()?.task.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("download worker disconnected".to_string()),
        };
        let pending = self.pending_download.take()?;
        Some(result.and_then(|response| {
            if !response.ok {
                return Err(format!(
                    "download failed with status {} {}",
                    response.status, response.status_text
                ));
            }
            for (url, cookie) in &response.set_cookies {
                crate::cookie_store_web::set_cookie_assignment_for_url(url, cookie, true);
            }
            let content_type = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.clone());
            Ok(Download {
                final_url: response.url,
                suggested_name: pending.suggested_name,
                content_type,
                bytes: response.body,
            })
        }))
    }

    fn install_reader_ui(&mut self) {
        if self.reader_ui_installed {
            return;
        }
        let body = crate::dom::body_id();
        let banner = crate::dom::create_element("div");
        crate::dom::set_attribute(banner, "id", "w3cos-reader-mode-indicator");
        crate::dom::set_attribute(banner, "role", "status");
        crate::dom::set_text_content(banner, "Reader mode · JavaScript disabled");
        crate::dom::set_style_property(banner, "padding", "10px 16px");
        crate::dom::set_style_property(banner, "background-color", "#fff3cd");
        crate::dom::set_style_property(banner, "color", "#332701");
        if let Some(first) = crate::dom::children(body).first().copied() {
            crate::dom::insert_before(body, banner, first);
        } else {
            crate::dom::append_child(body, banner);
        }
        crate::dom::set_style_property(body, "max-width", "760px");
        crate::dom::set_style_property(body, "margin", "0 auto");
        crate::dom::set_style_property(body, "line-height", "1.65");
        remove_reader_noise(body);
        self.reader_ui_installed = true;
    }
}

impl Drop for BrowserController {
    fn drop(&mut self) {
        if let Some(pending) = self.pending_download.take() {
            pending.task.cancel();
        }
    }
}

fn normalize_address(address: &str) -> Result<Url> {
    let address = address.trim();
    if address.is_empty() {
        return Err(anyhow!("address is empty"));
    }
    if let Ok(url) = Url::parse(address) {
        if matches!(url.scheme(), "http" | "https") {
            return Ok(url);
        }
        return Err(anyhow!("Browser navigation supports only HTTP(S)"));
    }
    Url::parse(&format!("https://{address}"))
        .map_err(|error| anyhow!("invalid Browser address {address:?}: {error}"))
}

fn is_same_document_navigation(current: &str, target: &str) -> bool {
    let (Ok(mut current), Ok(mut target)) = (Url::parse(current), Url::parse(target)) else {
        return false;
    };
    let different_fragment = current.fragment() != target.fragment();
    current.set_fragment(None);
    target.set_fragment(None);
    different_fragment && current == target
}

fn scroll_to_fragment(url: &Url) {
    let Some(fragment) = url.fragment().filter(|fragment| !fragment.is_empty()) else {
        return;
    };
    if let Some(node) = crate::dom::get_element_by_id(fragment) {
        crate::jsdom::element_value(node).call_method("scrollIntoView", vec![]);
    }
}

fn remove_reader_noise(root: u32) {
    for child in crate::dom::children(root) {
        let tag = crate::dom::tag_name(child);
        if matches!(tag.as_str(), "script" | "noscript" | "nav" | "footer")
            && crate::dom::get_attribute(child, "id").as_deref()
                != Some("w3cos-reader-mode-indicator")
        {
            crate::dom::remove_child(root, child);
        } else {
            remove_reader_noise(child);
        }
    }
}

fn sanitize_download_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| {
            if matches!(character, '/' | '\\' | '\0') {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.trim().is_empty() {
        "download".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_bar_adds_https_and_rejects_privileged_protocols() {
        assert_eq!(
            normalize_address("example.com/path").unwrap().as_str(),
            "https://example.com/path"
        );
        assert!(normalize_address("file:///etc/passwd").is_err());
    }

    #[test]
    fn download_names_cannot_escape_the_shell_destination() {
        assert_eq!(
            sanitize_download_name("../../report.pdf"),
            ".._.._report.pdf"
        );
    }

    #[test]
    fn fragment_navigation_is_classified_without_reloading() {
        assert!(is_same_document_navigation(
            "https://example.test/page#one",
            "https://example.test/page#two"
        ));
        assert!(!is_same_document_navigation(
            "https://example.test/page",
            "https://example.test/other#two"
        ));
    }
}
