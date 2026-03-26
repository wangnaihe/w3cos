/// W3C Location API — represents the URL of the current document.
///
/// In W3C OS there is no real URL bar; Location tracks a virtual URL
/// for SPA routing purposes.
#[derive(Debug, Clone)]
pub struct Location {
    protocol: String,
    hostname: String,
    port: String,
    pathname: String,
    search: String,
    hash: String,
}

impl Location {
    pub fn new(url: &str) -> Self {
        let mut loc = Self {
            protocol: "w3cos:".to_string(),
            hostname: "localhost".to_string(),
            port: String::new(),
            pathname: "/".to_string(),
            search: String::new(),
            hash: String::new(),
        };
        loc.parse_url(url);
        loc
    }

    fn parse_url(&mut self, url: &str) {
        let mut remaining = url;

        if let Some(proto_end) = remaining.find("://") {
            self.protocol = format!("{}:", &remaining[..proto_end]);
            remaining = &remaining[proto_end + 3..];
            if let Some(slash) = remaining.find('/') {
                let authority = &remaining[..slash];
                remaining = &remaining[slash..];
                if let Some(colon) = authority.rfind(':') {
                    self.hostname = authority[..colon].to_string();
                    self.port = authority[colon + 1..].to_string();
                } else {
                    self.hostname = authority.to_string();
                    self.port.clear();
                }
            } else {
                self.hostname = remaining.to_string();
                self.port.clear();
                remaining = "/";
            }
        }

        if let Some(hash_pos) = remaining.find('#') {
            self.hash = remaining[hash_pos..].to_string();
            remaining = &remaining[..hash_pos];
        } else {
            self.hash.clear();
        }

        if let Some(q_pos) = remaining.find('?') {
            self.search = remaining[q_pos..].to_string();
            remaining = &remaining[..q_pos];
        } else {
            self.search.clear();
        }

        self.pathname = if remaining.is_empty() {
            "/".to_string()
        } else {
            remaining.to_string()
        };
    }

    pub fn pathname(&self) -> &str {
        &self.pathname
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn href(&self) -> String {
        let authority = if self.port.is_empty() {
            self.hostname.clone()
        } else {
            format!("{}:{}", self.hostname, self.port)
        };
        format!(
            "//{}{}{}{}", authority, self.pathname, self.search, self.hash
        )
    }

    pub fn host(&self) -> String {
        if self.port.is_empty() {
            self.hostname.clone()
        } else {
            format!("{}:{}", self.hostname, self.port)
        }
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    pub fn origin(&self) -> String {
        format!("{}//{}", self.protocol, self.host())
    }

    pub fn set_href(&mut self, href: &str) {
        self.parse_url(href);
    }

    pub fn set_pathname(&mut self, pathname: &str) {
        self.pathname = pathname.to_string();
    }

    pub fn set_hash(&mut self, hash: &str) {
        if hash.starts_with('#') {
            self.hash = hash.to_string();
        } else if hash.is_empty() {
            self.hash.clear();
        } else {
            self.hash = format!("#{hash}");
        }
    }

    pub fn set_search(&mut self, search: &str) {
        if search.starts_with('?') {
            self.search = search.to_string();
        } else if search.is_empty() {
            self.search.clear();
        } else {
            self.search = format!("?{search}");
        }
    }
}

impl Default for Location {
    fn default() -> Self {
        Self::new("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_location() {
        let loc = Location::new("/");
        assert_eq!(loc.pathname(), "/");
        assert_eq!(loc.hash(), "");
        assert_eq!(loc.search(), "");
        assert_eq!(loc.hostname(), "localhost");
    }

    #[test]
    fn parse_path_with_query_and_hash() {
        let loc = Location::new("/page?q=hello#section");
        assert_eq!(loc.pathname(), "/page");
        assert_eq!(loc.search(), "?q=hello");
        assert_eq!(loc.hash(), "#section");
    }

    #[test]
    fn parse_full_url() {
        let loc = Location::new("https://example.com:8080/path?k=v#frag");
        assert_eq!(loc.protocol(), "https:");
        assert_eq!(loc.hostname(), "example.com");
        assert_eq!(loc.port(), "8080");
        assert_eq!(loc.pathname(), "/path");
        assert_eq!(loc.search(), "?k=v");
        assert_eq!(loc.hash(), "#frag");
        assert_eq!(loc.origin(), "https://example.com:8080");
    }

    #[test]
    fn set_hash() {
        let mut loc = Location::new("/page");
        loc.set_hash("#top");
        assert_eq!(loc.hash(), "#top");
        loc.set_hash("bottom");
        assert_eq!(loc.hash(), "#bottom");
        loc.set_hash("");
        assert_eq!(loc.hash(), "");
    }

    #[test]
    fn set_search() {
        let mut loc = Location::new("/page");
        loc.set_search("?foo=bar");
        assert_eq!(loc.search(), "?foo=bar");
        loc.set_search("baz=1");
        assert_eq!(loc.search(), "?baz=1");
    }

    #[test]
    fn set_pathname() {
        let mut loc = Location::new("/old");
        loc.set_pathname("/new");
        assert_eq!(loc.pathname(), "/new");
    }
}
