use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

use w3cos_core::Value;

use crate::streams::ReadableStream;

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

/// W3C `Response` — mirrors the Fetch API Response interface.
///
/// The body is a `ReadableStream` — call `.text()` / `.json()` for buffered
/// access, or `.body()` to get the raw stream for incremental consumption
/// (e.g. SSE, streaming LLM responses).
pub struct FetchResponse {
    pub status: u16,
    pub ok: bool,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    /// The response body as a `ReadableStream<Uint8Array>`.
    /// Consumed once — matches the W3C spec's "body used" flag.
    body_stream: ReadableStream,
}

impl FetchResponse {
    /// `Response.body` — the raw `ReadableStream`.
    /// Use this for streaming / incremental consumption.
    pub fn body(&self) -> &ReadableStream {
        &self.body_stream
    }

    /// `Response.text()` — buffer the entire body as a UTF-8 string.
    /// Blocks until the stream is fully consumed.
    pub fn text(&self) -> Result<String, String> {
        let reader = self.body_stream.get_reader();
        reader.read_to_string()
    }

    /// `Response.arrayBuffer()` — buffer the entire body as raw bytes.
    pub fn array_buffer(&self) -> Result<Vec<u8>, String> {
        let reader = self.body_stream.get_reader();
        reader.read_to_end()
    }

    /// `Response.json()` — buffer and parse the body as JSON.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        let text = self.text()?;
        serde_json::from_str(&text).map_err(|e| format!("JSON parse error: {e}"))
    }

    /// Convenience: clone headers without consuming the body.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|s| s.as_str())
    }
}

/// Blocking fetch — performs an HTTP request and returns a `FetchResponse`.
/// The response body is streamed lazily via `ReadableStream`.
pub fn fetch(url: &str, options: FetchOptions) -> FetchResponse {
    match fetch_inner(url, &options) {
        Ok(resp) => resp,
        Err(e) => FetchResponse {
            status: 0,
            ok: false,
            status_text: e.to_string(),
            headers: HashMap::new(),
            body_stream: ReadableStream::from_bytes(Vec::new()),
        },
    }
}

/// JavaScript-facing synchronous `fetch` facade used by the ESM AOT pipeline.
///
/// The current ESM lowering executes `await` expressions synchronously, so
/// returning a browser-shaped `Response` value here keeps `response.ok`,
/// `response.status`, `response.text()` and `response.json()` available to
/// compiled application code.
pub fn fetch_value(arguments: Vec<Value>) -> Value {
    let url = arguments
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let init = arguments.get(1).cloned().unwrap_or(Value::Undefined);
    let mut options = FetchOptions {
        method: parse_method(&init.get_property("method").to_js_string()),
        body: match init.get_property("body") {
            Value::Undefined | Value::Null => None,
            body => Some(body.to_js_string()),
        },
        ..FetchOptions::default()
    };
    if let Value::Object(headers) = init.get_property("headers") {
        let headers = headers.borrow();
        for key in headers.keys() {
            options
                .headers
                .insert(key.clone(), headers.get_direct(&key).to_js_string());
        }
    }

    response_value(fetch(&url, options))
}

fn response_value(response: FetchResponse) -> Value {
    let status = response.status;
    let ok = response.ok;
    let status_text = response.status_text.clone();
    let headers = Value::object(
        response
            .headers
            .iter()
            .map(|(key, value)| (key.clone(), Value::from(value.clone())))
            .collect(),
    );
    let body = response.text().unwrap_or_default();
    let text_body = body.clone();
    let json_body = body;

    Value::object(HashMap::from([
        ("status".into(), Value::Number(status as f64)),
        ("ok".into(), Value::Bool(ok)),
        ("statusText".into(), Value::from(status_text)),
        ("headers".into(), headers),
        (
            "text".into(),
            Value::function(move |_, _| Value::from(text_body.clone())),
        ),
        (
            "json".into(),
            Value::function(move |_, _| {
                w3cos_core::json::parse(vec![Value::from(json_body.clone())])
            }),
        ),
    ]))
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

fn fetch_inner(
    url: &str,
    options: &FetchOptions,
) -> Result<FetchResponse, Box<dyn std::error::Error>> {
    let agent = build_agent(options);

    let build_without_body =
        |mut req: ureq::RequestBuilder<ureq::typestate::WithoutBody>| -> Result<
            ureq::http::Response<ureq::Body>,
            ureq::Error,
        > {
            for (key, value) in &options.headers {
                req = req.header(key.as_str(), value.as_str());
            }
            req.call()
        };

    let build_with_body =
        |mut req: ureq::RequestBuilder<ureq::typestate::WithBody>| -> Result<
            ureq::http::Response<ureq::Body>,
            ureq::Error,
        > {
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

    // Wrap the response body in a ReadableStream — streamed in 16 KiB chunks.
    // The reader runs on a background thread so the caller is never blocked
    // waiting for the full body before processing begins.
    let body_reader = resp.into_body().into_reader();
    let body_stream = ReadableStream::from_reader(body_reader, 16 * 1024);

    Ok(FetchResponse {
        status,
        ok: (200..300).contains(&status),
        status_text,
        headers,
        body_stream,
    })
}

pub enum FetchResult {
    Success(FetchResponse),
    Error(String),
}

/// Non-blocking fetch — runs the request in a background thread.
/// Returns a channel receiver that yields a single `FetchResult`.
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
    fn fetch_value_exposes_browser_response_properties() {
        let response = response_value(FetchResponse {
            status: 200,
            ok: true,
            status_text: "OK".into(),
            headers: HashMap::from([("content-type".into(), "application/json".into())]),
            body_stream: ReadableStream::from_bytes(br#"{"token":"ok"}"#.to_vec()),
        });

        assert!(response.get_property("ok").to_bool());
        assert_eq!(response.get_property("status").to_number(), 200.0);
        assert_eq!(
            response
                .call_method("json", vec![])
                .get_property("token")
                .to_js_string(),
            "ok"
        );
    }

    #[test]
    fn fetch_get_httpbin() {
        let resp = fetch("https://httpbin.org/get", FetchOptions::default());
        assert!(resp.ok, "status: {} {}", resp.status, resp.status_text);
        assert_eq!(resp.status, 200);
        assert!(!resp.text().unwrap().is_empty());
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
        let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        match result {
            FetchResult::Success(resp) => {
                assert!(resp.ok);
                assert_eq!(resp.status, 200);
            }
            FetchResult::Error(e) => panic!("fetch failed: {e}"),
        }
    }
}
