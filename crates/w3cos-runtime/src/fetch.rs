use std::collections::HashMap;
use std::io::Read as IoRead;
use std::sync::mpsc;
use std::thread;

#[derive(Debug, Clone, Default)]
pub enum Method {
    #[default]
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
}

#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    pub method: Method,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub status: u16,
    pub ok: bool,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    body: String,
}

impl FetchResponse {
    pub fn text(&self) -> &str {
        &self.body
    }

    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(&self.body).map_err(|e| format!("JSON parse error: {e}"))
    }
}

/// Blocking fetch — performs an HTTP request and returns the response.
pub fn fetch(url: &str, options: FetchOptions) -> FetchResponse {
    match fetch_inner(url, &options) {
        Ok(resp) => resp,
        Err(e) => FetchResponse {
            status: 0,
            ok: false,
            status_text: e.to_string(),
            headers: HashMap::new(),
            body: String::new(),
        },
    }
}

fn build_agent(options: &FetchOptions) -> ureq::Agent {
    if let Some(ms) = options.timeout_ms {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_millis(ms)))
            .build();
        ureq::Agent::new_with_config(config)
    } else {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(30)))
                .build(),
        )
    }
}

fn fetch_inner(url: &str, options: &FetchOptions) -> Result<FetchResponse, Box<dyn std::error::Error>> {
    let agent = build_agent(options);

    let build_without_body = |mut req: ureq::RequestBuilder<ureq::typestate::WithoutBody>| -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        for (key, value) in &options.headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req.call()
    };

    let build_with_body = |mut req: ureq::RequestBuilder<ureq::typestate::WithBody>| -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        for (key, value) in &options.headers {
            req = req.header(key.as_str(), value.as_str());
        }
        if let Some(ref body) = options.body {
            if !options.headers.contains_key("content-type")
                && !options.headers.contains_key("Content-Type")
            {
                req = req.header("Content-Type", "application/json");
            }
            req.send(body.as_bytes())
        } else {
            req.send_empty()
        }
    };

    let resp = match options.method {
        Method::Get => build_without_body(agent.get(url))?,
        Method::Post => build_with_body(agent.post(url))?,
        Method::Put => build_with_body(agent.put(url))?,
        Method::Delete => build_without_body(agent.delete(url))?,
        Method::Patch => build_with_body(agent.patch(url))?,
        Method::Head => build_without_body(agent.head(url))?,
        Method::Options => build_without_body(agent.options(url))?,
    };

    let status = resp.status().as_u16();
    let status_text = resp
        .status()
        .canonical_reason()
        .unwrap_or("Unknown")
        .to_string();

    let mut headers = HashMap::new();
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.to_string(), v.to_string());
        }
    }

    let mut body_str = String::new();
    resp.into_body()
        .as_reader()
        .read_to_string(&mut body_str)?;

    Ok(FetchResponse {
        status,
        ok: (200..300).contains(&status),
        status_text,
        headers,
        body: body_str,
    })
}

pub enum FetchResult {
    Success(FetchResponse),
    Error(String),
}

/// Non-blocking fetch — runs the request in a background thread.
pub fn fetch_async(url: &str, options: FetchOptions) -> mpsc::Receiver<FetchResult> {
    let (tx, rx) = mpsc::channel();
    let url = url.to_string();

    thread::spawn(move || {
        let result = match fetch_inner(&url, &options) {
            Ok(resp) => FetchResult::Success(resp),
            Err(e) => FetchResult::Error(e.to_string()),
        };
        let _ = tx.send(result);
    });

    rx
}

pub fn parse_method(s: &str) -> Method {
    match s.to_uppercase().as_str() {
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "PATCH" => Method::Patch,
        "HEAD" => Method::Head,
        "OPTIONS" => Method::Options,
        _ => Method::Get,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_get_httpbin() {
        let resp = fetch("https://httpbin.org/get", FetchOptions::default());
        assert!(resp.ok, "status: {} {}", resp.status, resp.status_text);
        assert_eq!(resp.status, 200);
        assert!(!resp.text().is_empty());
    }

    #[test]
    fn fetch_post_json() {
        let resp = fetch(
            "https://httpbin.org/post",
            FetchOptions {
                method: Method::Post,
                body: Some(r#"{"hello":"w3cos"}"#.to_string()),
                ..Default::default()
            },
        );
        assert!(resp.ok);
        let json = resp.json().unwrap();
        assert!(json["data"].as_str().unwrap().contains("w3cos"));
    }

    #[test]
    fn fetch_invalid_url() {
        let resp = fetch(
            "https://this-domain-does-not-exist-w3cos.invalid/",
            FetchOptions {
                timeout_ms: Some(3000),
                ..Default::default()
            },
        );
        assert!(!resp.ok);
        assert_eq!(resp.status, 0);
    }

    #[test]
    fn fetch_async_works() {
        let rx = fetch_async("https://httpbin.org/get", FetchOptions::default());
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        match result {
            FetchResult::Success(resp) => {
                assert!(resp.ok);
                assert_eq!(resp.status, 200);
            }
            FetchResult::Error(e) => panic!("fetch failed: {e}"),
        }
    }
}
