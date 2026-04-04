use std::collections::HashMap;
use std::path::PathBuf;

/// W3C OS App manifest — describes an installed application.
/// Loaded from `w3cos.json` in the app directory.
#[derive(Debug, Clone)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub icon: Option<String>,
    pub permissions: Vec<String>,
    pub window: WindowConfig,
}

#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub default_width: u32,
    pub default_height: u32,
    pub min_width: u32,
    pub min_height: u32,
    pub resizable: bool,
    pub frame: bool,
    pub title: Option<String>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            default_width: 1024,
            default_height: 768,
            min_width: 320,
            min_height: 240,
            resizable: true,
            frame: true,
            title: None,
        }
    }
}

impl Default for AppManifest {
    fn default() -> Self {
        Self {
            id: "app".to_string(),
            name: "W3C OS App".to_string(),
            version: "0.1.0".to_string(),
            entry: "app.tsx".to_string(),
            icon: None,
            permissions: Vec::new(),
            window: WindowConfig::default(),
        }
    }
}

/// Parsed w3cos:// URL.
#[derive(Debug, Clone)]
pub struct W3cosUrl {
    pub app_id: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub fragment: String,
}

impl W3cosUrl {
    /// Parse a w3cos:// URL into its components.
    ///
    /// Examples:
    ///   w3cos://files              → app_id="files", path="/"
    ///   w3cos://files/home/user    → app_id="files", path="/home/user"
    ///   w3cos://editor?file=a.tsx  → app_id="editor", query={"file":"a.tsx"}
    ///   w3cos://settings/display   → app_id="settings", path="/display"
    pub fn parse(url: &str) -> Option<Self> {
        let stripped = url.strip_prefix("w3cos://")?;

        let mut remaining = stripped;
        let mut fragment = String::new();
        let mut query = HashMap::new();

        if let Some(hash_pos) = remaining.find('#') {
            fragment = remaining[hash_pos + 1..].to_string();
            remaining = &remaining[..hash_pos];
        }

        if let Some(q_pos) = remaining.find('?') {
            let query_str = &remaining[q_pos + 1..];
            for pair in query_str.split('&') {
                if let Some(eq) = pair.find('=') {
                    query.insert(
                        pair[..eq].to_string(),
                        pair[eq + 1..].to_string(),
                    );
                } else if !pair.is_empty() {
                    query.insert(pair.to_string(), String::new());
                }
            }
            remaining = &remaining[..q_pos];
        }

        let (app_id, path) = if let Some(slash) = remaining.find('/') {
            (
                remaining[..slash].to_string(),
                remaining[slash..].to_string(),
            )
        } else {
            (remaining.to_string(), "/".to_string())
        };

        if app_id.is_empty() {
            return None;
        }

        Some(Self {
            app_id,
            path,
            query,
            fragment,
        })
    }

    /// Convert back to a URL string.
    pub fn to_href(&self) -> String {
        let mut url = format!("w3cos://{}{}", self.app_id, self.path);
        if !self.query.is_empty() {
            let pairs: Vec<String> = self
                .query
                .iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        k.clone()
                    } else {
                        format!("{k}={v}")
                    }
                })
                .collect();
            url.push('?');
            url.push_str(&pairs.join("&"));
        }
        if !self.fragment.is_empty() {
            url.push('#');
            url.push_str(&self.fragment);
        }
        url
    }
}

/// App registry — maps app IDs to their manifests.
#[derive(Debug, Default)]
pub struct AppRegistry {
    apps: HashMap<String, AppManifest>,
}

impl AppRegistry {
    pub fn new() -> Self {
        Self {
            apps: HashMap::new(),
        }
    }

    pub fn register(&mut self, manifest: AppManifest) {
        self.apps.insert(manifest.id.clone(), manifest);
    }

    pub fn get(&self, app_id: &str) -> Option<&AppManifest> {
        self.apps.get(app_id)
    }

    pub fn list(&self) -> Vec<&AppManifest> {
        self.apps.values().collect()
    }

    /// Register built-in system apps.
    pub fn register_builtins(&mut self) {
        let builtins = [
            ("shell", "Desktop Shell", "◆"),
            ("files", "File Manager", "📁"),
            ("terminal", "Terminal", "⌨"),
            ("settings", "Settings", "⚙"),
            ("ai-agent", "AI Agent", "🤖"),
            ("editor", "Editor", "📝"),
        ];
        for (id, name, icon) in builtins {
            self.register(AppManifest {
                id: id.to_string(),
                name: name.to_string(),
                icon: Some(icon.to_string()),
                ..Default::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_url() {
        let url = W3cosUrl::parse("w3cos://files").unwrap();
        assert_eq!(url.app_id, "files");
        assert_eq!(url.path, "/");
        assert!(url.query.is_empty());
    }

    #[test]
    fn parse_url_with_path() {
        let url = W3cosUrl::parse("w3cos://files/home/user/Documents").unwrap();
        assert_eq!(url.app_id, "files");
        assert_eq!(url.path, "/home/user/Documents");
    }

    #[test]
    fn parse_url_with_query() {
        let url = W3cosUrl::parse("w3cos://editor?file=app.tsx&line=42").unwrap();
        assert_eq!(url.app_id, "editor");
        assert_eq!(url.query.get("file"), Some(&"app.tsx".to_string()));
        assert_eq!(url.query.get("line"), Some(&"42".to_string()));
    }

    #[test]
    fn parse_url_with_fragment() {
        let url = W3cosUrl::parse("w3cos://settings/display#theme").unwrap();
        assert_eq!(url.app_id, "settings");
        assert_eq!(url.path, "/display");
        assert_eq!(url.fragment, "theme");
    }

    #[test]
    fn parse_url_full() {
        let url = W3cosUrl::parse("w3cos://ai-agent?agent=code&task=review#output").unwrap();
        assert_eq!(url.app_id, "ai-agent");
        assert_eq!(url.query.get("agent"), Some(&"code".to_string()));
        assert_eq!(url.fragment, "output");
    }

    #[test]
    fn roundtrip() {
        let url = W3cosUrl::parse("w3cos://files/home/user").unwrap();
        assert!(url.to_href().starts_with("w3cos://files/home/user"));
    }

    #[test]
    fn invalid_url() {
        assert!(W3cosUrl::parse("https://example.com").is_none());
        assert!(W3cosUrl::parse("w3cos://").is_none());
    }

    #[test]
    fn registry_builtins() {
        let mut reg = AppRegistry::new();
        reg.register_builtins();
        assert!(reg.get("files").is_some());
        assert!(reg.get("terminal").is_some());
        assert_eq!(reg.get("files").unwrap().name, "File Manager");
        assert_eq!(reg.list().len(), 6);
    }
}
