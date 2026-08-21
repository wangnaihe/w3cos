use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread;

use w3cos_core::Value;

use crate::jsdom::realm_function;
use crate::streams::ReadableStream;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ScriptReferrerPolicy {
    NoReferrer,
    NoReferrerWhenDowngrade,
    Origin,
    OriginWhenCrossOrigin,
    SameOrigin,
    StrictOrigin,
    StrictOriginWhenCrossOrigin,
    UnsafeUrl,
}

impl Default for ScriptReferrerPolicy {
    fn default() -> Self {
        Self::StrictOriginWhenCrossOrigin
    }
}

impl ScriptReferrerPolicy {
    pub(crate) fn parse(value: Option<&str>) -> Self {
        value.and_then(Self::parse_token).unwrap_or_default()
    }

    fn parse_token(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "no-referrer" => Some(Self::NoReferrer),
            "no-referrer-when-downgrade" => Some(Self::NoReferrerWhenDowngrade),
            "origin" => Some(Self::Origin),
            "origin-when-cross-origin" => Some(Self::OriginWhenCrossOrigin),
            "same-origin" => Some(Self::SameOrigin),
            "strict-origin" => Some(Self::StrictOrigin),
            "strict-origin-when-cross-origin" => Some(Self::StrictOriginWhenCrossOrigin),
            "unsafe-url" => Some(Self::UnsafeUrl),
            _ => None,
        }
    }

    fn from_header(value: &str) -> Option<Self> {
        value.split(',').filter_map(Self::parse_token).next_back()
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoReferrer => "no-referrer",
            Self::NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
            Self::Origin => "origin",
            Self::OriginWhenCrossOrigin => "origin-when-cross-origin",
            Self::SameOrigin => "same-origin",
            Self::StrictOrigin => "strict-origin",
            Self::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
            Self::UnsafeUrl => "unsafe-url",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BrowserCredentialsMode {
    Omit,
    SameOrigin,
    Include,
}

const MAX_CORS_PREFLIGHT_CACHE_ENTRIES: usize = 128;
const MAX_CORS_PREFLIGHT_CACHE_AGE_SECS: u64 = 7_200;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CorsPreflightCacheKey {
    request_origin: String,
    target_origin: String,
    credentials_mode: BrowserCredentialsMode,
}

struct CorsPreflightCacheEntry {
    allowed_methods: HashSet<String>,
    allowed_headers: HashSet<String>,
    method_wildcard: bool,
    header_wildcard: bool,
    expires_at: std::time::Instant,
    last_used: u64,
}

#[derive(Default)]
struct CorsPreflightCache {
    entries: HashMap<CorsPreflightCacheKey, CorsPreflightCacheEntry>,
    clock: u64,
}

static CORS_PREFLIGHT_CACHE: OnceLock<Mutex<CorsPreflightCache>> = OnceLock::new();

fn cors_preflight_cache() -> &'static Mutex<CorsPreflightCache> {
    CORS_PREFLIGHT_CACHE.get_or_init(|| Mutex::new(CorsPreflightCache::default()))
}

thread_local! {
    static PAGE_HTTP_CACHE_POLICY: RefCell<Option<crate::browser_http_cache::CachePolicy>> =
        const { RefCell::new(None) };
    static HEADERS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static RESPONSE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static REQUEST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FETCH_FUNCTION: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn realm_fetch_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserRequestMode {
    Cors,
    SameOrigin,
    NoCors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserRedirectMode {
    Follow,
    Error,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserCacheMode {
    Default,
    NoStore,
    Reload,
    NoCache,
    ForceCache,
    OnlyIfCached,
}

pub(crate) fn set_page_http_cache_policy(policy: crate::browser_http_cache::CachePolicy) {
    PAGE_HTTP_CACHE_POLICY.with(|active| {
        *active.borrow_mut() = Some(policy);
    });
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
    pub url: String,
    pub redirected: bool,
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
            url: url.to_string(),
            redirected: false,
            body_stream: ReadableStream::from_bytes(Vec::new()),
        },
    }
}

/// Interned JavaScript `fetch` callable.
///
/// ESM lowering clones this instead of allocating a fresh `Value::function`
/// on every global read. The host I/O behind [`fetch_value`] stays blocking;
/// W3IR AOT `await` suspends through Promise reactions, not a Tokio runtime.
pub fn fetch_function() -> Value {
    FETCH_FUNCTION.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| Value::function(|_this, arguments| fetch_value(arguments)))
            .clone()
    })
}

/// JavaScript-facing `fetch` facade used by the ESM AOT pipeline.
///
/// Typed ESM lowering emits Rust `.await`. W3IR AOT `await` suspends through
/// `Frame::drive` Promise reactions and takes a fulfilled-promise fast path
/// when `promise::status` is already `Fulfilled`. This helper still returns a
/// thenable Response facade so `fetch(...).then(...)` and property reads
/// (`response.ok`) share one object. The I/O itself remains host-blocking
/// (`ureq`); it is not rewritten into a Tokio future.
pub fn fetch_value(arguments: Vec<Value>) -> Value {
    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
    let init = arguments.get(1).cloned().unwrap_or(Value::Undefined);
    let request_like = !input.get_property("url").is_undefined();
    let requested_url = if request_like {
        input.get_property("url").to_js_string()
    } else {
        input.to_js_string()
    };
    let base_method = if request_like {
        input.get_property("method")
    } else {
        Value::Undefined
    };
    let method = if init.get_property("method").is_undefined() {
        base_method
    } else {
        init.get_property("method")
    };
    let base_body = if request_like {
        input.get_property("__w3cos_body")
    } else {
        Value::Undefined
    };
    let body = if init.get_property("body").is_undefined() {
        base_body
    } else {
        init.get_property("body")
    };
    let base_headers = if request_like {
        input.get_property("headers")
    } else {
        Value::Undefined
    };
    let headers = if init.get_property("headers").is_undefined() {
        base_headers
    } else {
        init.get_property("headers")
    };
    let base_cache = if request_like {
        input.get_property("cache")
    } else {
        Value::Undefined
    };
    let cache = if init.get_property("cache").is_undefined() {
        base_cache
    } else {
        init.get_property("cache")
    };
    let base_signal = if request_like {
        input.get_property("signal")
    } else {
        Value::Undefined
    };
    let signal = if init.get_property("signal").is_undefined() {
        base_signal
    } else {
        init.get_property("signal")
    };
    let base_credentials = if request_like {
        input.get_property("credentials")
    } else {
        Value::Undefined
    };
    let credentials = if init.get_property("credentials").is_undefined() {
        base_credentials
    } else {
        init.get_property("credentials")
    };
    let base_mode = if request_like {
        input.get_property("mode")
    } else {
        Value::Undefined
    };
    let mode = if init.get_property("mode").is_undefined() {
        base_mode
    } else {
        init.get_property("mode")
    };
    let base_redirect = if request_like {
        input.get_property("redirect")
    } else {
        Value::Undefined
    };
    let redirect = if init.get_property("redirect").is_undefined() {
        base_redirect
    } else {
        init.get_property("redirect")
    };
    if signal.get_property("aborted").to_bool() {
        let reason = signal.get_property("reason");
        let reason = if reason.is_undefined() {
            "The operation was aborted.".to_string()
        } else {
            reason.to_js_string()
        };
        eprintln!("W3COS warning: fetch was cancelled before native I/O: {reason}");
        return fetch_promise_facade(fetch_error_value(&requested_url, "AbortError", &reason));
    }
    if let Some((bytes, media_type)) = w3cos_core::web::object_url_resource(&requested_url) {
        let headers = if media_type.is_empty() {
            Vec::new()
        } else {
            vec![("content-type".into(), media_type)]
        };
        return fetch_promise_facade(response_from_bytes(
            bytes,
            200,
            "OK".into(),
            headers_value_from_list(Rc::new(RefCell::new(headers))),
            requested_url,
            "basic".into(),
        ));
    }
    if requested_url.starts_with("blob:w3cos/") {
        return fetch_promise_facade(fetch_error_value(
            &requested_url,
            "NetworkError",
            "the object URL has been revoked or does not exist",
        ));
    }
    let url = match resolve_page_fetch_url(&requested_url) {
        Ok(url) => url,
        Err(error) => {
            return fetch_promise_facade(fetch_error_value(&requested_url, "NetworkError", &error));
        }
    };
    let multipart = crate::form_data::serialize(&body);
    let mut options = FetchOptions {
        method: parse_method(&method.to_js_string()),
        body: multipart
            .as_ref()
            .map(|(body, _)| body.clone())
            .or_else(|| match body {
                Value::Undefined | Value::Null => None,
                body => Some(body.to_js_string()),
            }),
        ..FetchOptions::default()
    };
    options.headers = headers_to_map(&headers);
    if let Some((_, content_type)) = multipart {
        options
            .headers
            .entry("content-type".to_string())
            .or_insert(content_type);
    }
    let timeout = init.get_property("timeout");
    if timeout.is_number() {
        options.timeout_ms = Some(timeout.to_number().max(0.0) as u64);
    } else {
        let signal_timeout = signal.get_property("__w3cos_timeout_ms");
        if signal_timeout.is_number() {
            options.timeout_ms = Some(signal_timeout.to_number().max(0.0) as u64);
        }
    }

    let credentials_mode = match credentials.to_js_string().as_str() {
        "omit" => BrowserCredentialsMode::Omit,
        "include" => BrowserCredentialsMode::Include,
        _ => BrowserCredentialsMode::SameOrigin,
    };
    let request_mode = match mode.to_js_string().as_str() {
        "same-origin" => BrowserRequestMode::SameOrigin,
        "no-cors" => BrowserRequestMode::NoCors,
        _ => BrowserRequestMode::Cors,
    };
    let redirect_mode = match redirect.to_js_string().as_str() {
        "error" => BrowserRedirectMode::Error,
        "manual" => BrowserRedirectMode::Manual,
        _ => BrowserRedirectMode::Follow,
    };
    let cache_mode = match cache.to_js_string().as_str() {
        "no-store" => BrowserCacheMode::NoStore,
        "reload" => BrowserCacheMode::Reload,
        "no-cache" => BrowserCacheMode::NoCache,
        "force-cache" => BrowserCacheMode::ForceCache,
        "only-if-cached" => BrowserCacheMode::OnlyIfCached,
        _ => BrowserCacheMode::Default,
    };
    let response = match fetch_page_response(
        &url,
        options,
        credentials_mode,
        request_mode,
        redirect_mode,
        cache_mode,
        &signal,
    ) {
        Ok((response, response_type)) => {
            let value = response_value(response, url);
            value.set_property("type", Value::from(response_type));
            value
        }
        Err(error) => {
            if signal.get_property("aborted").to_bool() {
                fetch_abort_error_value(&url, &signal)
            } else {
                fetch_error_value(&url, "NetworkError", &error)
            }
        }
    };
    fetch_promise_facade(response)
}

fn fetch_abort_error_value(url: &str, signal: &Value) -> Value {
    let reason = signal.get_property("reason");
    let reason_name = reason.get_property("name").to_js_string();
    let reason_text = if reason.is_undefined() {
        "The operation was aborted.".to_string()
    } else {
        reason.to_js_string()
    };
    let name = if reason_name == "TimeoutError" || reason_text == "TimeoutError" {
        "TimeoutError"
    } else {
        "AbortError"
    };
    fetch_error_value(url, name, &reason_text)
}

pub(crate) fn resolve_page_fetch_url(requested_url: &str) -> Result<String, String> {
    if let Ok(url) = url::Url::parse(requested_url) {
        return Ok(url.to_string());
    }
    let base = crate::cookie_store_web::active_document_url();
    url::Url::parse(&base)
        .map_err(|error| format!("the active document URL is invalid: {error}"))?
        .join(requested_url)
        .map(|url| url.to_string())
        .map_err(|error| format!("the request URL is invalid: {error}"))
}

#[cfg(feature = "dynamic-js")]
pub(crate) fn fetch_page_font_bytes(
    requested_url: &str,
    max_source_bytes: usize,
) -> Result<(Vec<u8>, String, String), String> {
    let url = resolve_page_fetch_url(requested_url)?;
    let mut options = FetchOptions::default();
    options.headers.insert(
        "Accept".to_string(),
        "font/woff2,font/woff,font/ttf,font/otf".to_string(),
    );
    let (response, _) = fetch_page_response(
        &url,
        options,
        BrowserCredentialsMode::Omit,
        BrowserRequestMode::Cors,
        BrowserRedirectMode::Follow,
        BrowserCacheMode::Default,
        &Value::Undefined,
    )?;
    if !response.ok {
        return Err(format!(
            "FontFace fetch failed with status {} {}",
            response.status, response.status_text
        ));
    }
    let content_type = response
        .header("content-type")
        .unwrap_or_default()
        .to_string();
    let final_url = response.url.clone();
    let body = response.array_buffer()?;
    if body.len() > max_source_bytes {
        return Err(format!(
            "FontFace exceeds source limit ({} > {} bytes)",
            body.len(),
            max_source_bytes
        ));
    }
    Ok((body, content_type, final_url))
}

fn fetch_page_response(
    url: &str,
    mut options: FetchOptions,
    credentials_mode: BrowserCredentialsMode,
    request_mode: BrowserRequestMode,
    redirect_mode: BrowserRedirectMode,
    cache_mode: BrowserCacheMode,
    signal: &Value,
) -> Result<(FetchResponse, &'static str), String> {
    let Some(document_url) = crate::cookie_store_web::active_document_url_if_set() else {
        if signal.is_undefined() {
            return Ok((fetch(url, options), "basic"));
        }
        return fetch_native_interruptible(url, options, signal);
    };
    let document = url::Url::parse(&document_url)
        .map_err(|error| format!("the active document URL is invalid: {error}"))?;
    if !matches!(document.scheme(), "http" | "https") {
        return Ok((fetch(url, options), "basic"));
    }
    if request_mode == BrowserRequestMode::NoCors {
        validate_no_cors_request(&options)?;
    }
    if cache_mode == BrowserCacheMode::OnlyIfCached
        && request_mode != BrowserRequestMode::SameOrigin
    {
        return Err("Fetch cache mode only-if-cached requires mode same-origin".to_string());
    }
    let request_origin = document.origin().ascii_serialization();
    let request_url =
        url::Url::parse(url).map_err(|error| format!("the request URL is invalid: {error}"))?;
    let target_origin = request_url.origin().ascii_serialization();
    let same_target_origin = target_origin == request_origin;
    let credentials_allowed = match credentials_mode {
        BrowserCredentialsMode::Omit => false,
        BrowserCredentialsMode::SameOrigin => same_target_origin,
        BrowserCredentialsMode::Include => true,
    };
    let cookies = crate::cookie_store_web::snapshot();
    let request_headers = browser_request_headers(
        &options,
        &request_url,
        &request_origin,
        &cookies,
        credentials_allowed,
        request_mode == BrowserRequestMode::Cors,
        &document_url,
        ScriptReferrerPolicy::default(),
        same_target_origin,
        false,
    );
    let cache_policy = PAGE_HTTP_CACHE_POLICY.with(|active| active.borrow().clone());
    let cache_key = page_http_cache_key(
        url,
        &request_origin,
        &target_origin,
        credentials_mode,
        request_mode,
    );
    let cacheable = options.method == Method::Get
        && options.body.is_none()
        && redirect_mode == BrowserRedirectMode::Follow
        && !(request_mode == BrowserRequestMode::NoCors && !same_target_origin);
    let may_read_cache = cacheable
        && !matches!(
            cache_mode,
            BrowserCacheMode::NoStore | BrowserCacheMode::Reload
        );
    let cached = if may_read_cache {
        cache_policy.as_ref().and_then(|policy| {
            crate::browser_http_cache::load(policy, &cache_key, &request_headers)
                .ok()
                .flatten()
        })
    } else {
        None
    };
    if cache_mode == BrowserCacheMode::OnlyIfCached && cached.is_none() {
        return Err("Fetch only-if-cached request did not match a stored response".to_string());
    }
    let use_cached_without_network = matches!(
        cache_mode,
        BrowserCacheMode::ForceCache | BrowserCacheMode::OnlyIfCached
    ) && cached.is_some();
    if !use_cached_without_network && let Some(cached) = cached.as_ref() {
        crate::browser_http_cache::add_revalidation_headers(&mut options.headers, cached);
    }
    let cancellation = FetchCancellation::default();
    let mut response = if use_cached_without_network {
        BrowserFetchResponse::from_cached(cached.expect("cached response checked above"))
    } else {
        let response = fetch_page_bytes_interruptible(
            url,
            options.clone(),
            request_origin.clone(),
            cookies.clone(),
            credentials_mode,
            request_mode,
            redirect_mode,
            document_url.clone(),
            cancellation.clone(),
            signal,
        )?;
        if response.status == 304 && cached.is_some() {
            let cached = cached.expect("cached response checked above");
            let merged = crate::browser_http_cache::merge_not_modified(
                cached,
                &response.url,
                &response.headers,
            )
            .map_err(|error| error.to_string())?;
            let mut merged = BrowserFetchResponse::from_cached(merged);
            merged.set_cookies = response.set_cookies;
            merged
        } else {
            response
        }
    };
    if request_mode == BrowserRequestMode::Cors {
        validate_browser_cors_headers(
            &request_origin,
            &response.url,
            &response.headers,
            credentials_mode,
        )
        .map_err(|error| format!("Fetch CORS check failed for {}: {error}", response.url))?;
    }
    for (cookie_url, assignment) in &response.set_cookies {
        crate::cookie_store_web::set_cookie_assignment_for_url(cookie_url, assignment, true);
    }
    if cacheable
        && cache_mode != BrowserCacheMode::NoStore
        && !use_cached_without_network
        && response.ok
        && !response.redirected
        && response.url == request_url.as_str()
        && let Some(policy) = cache_policy.as_ref()
    {
        let cached_response = crate::browser_http_cache::CachedResponse::from_network(
            &cache_key,
            response.url.clone(),
            response.status,
            response.status_text.clone(),
            response.headers.clone(),
            response.body.clone(),
        );
        let _ = crate::browser_http_cache::store(
            policy,
            &cache_key,
            &request_headers,
            cached_response,
            true,
        );
    }
    if redirect_mode == BrowserRedirectMode::Manual && is_redirect_status(response.status) {
        return Ok((
            FetchResponse {
                status: 0,
                ok: false,
                status_text: String::new(),
                headers: HashMap::new(),
                url: response.url,
                redirected: false,
                body_stream: ReadableStream::from_bytes(Vec::new()),
            },
            "opaqueredirect",
        ));
    }
    let same_origin = url::Url::parse(&response.url)
        .is_ok_and(|url| url.origin().ascii_serialization() == request_origin);
    if request_mode == BrowserRequestMode::NoCors && !same_origin {
        return Ok((
            FetchResponse {
                status: 0,
                ok: false,
                status_text: String::new(),
                headers: HashMap::new(),
                url: String::new(),
                redirected: response.redirected,
                body_stream: ReadableStream::from_bytes(Vec::new()),
            },
            "opaque",
        ));
    }
    filter_page_response_headers(&mut response.headers, !same_origin, credentials_mode);
    let response_type = if same_origin { "basic" } else { "cors" };
    Ok((
        FetchResponse {
            status: response.status,
            ok: response.ok,
            status_text: response.status_text,
            headers: response.headers,
            url: response.url,
            redirected: response.redirected,
            body_stream: ReadableStream::from_bytes(response.body),
        },
        response_type,
    ))
}

fn page_http_cache_key(
    request_url: &str,
    request_origin: &str,
    target_origin: &str,
    credentials_mode: BrowserCredentialsMode,
    request_mode: BrowserRequestMode,
) -> crate::browser_http_cache::CacheKey {
    let credentials = match credentials_mode {
        BrowserCredentialsMode::Omit => "omit",
        BrowserCredentialsMode::SameOrigin => "same-origin",
        BrowserCredentialsMode::Include => "include",
    };
    let mode = match request_mode {
        BrowserRequestMode::Cors => "cors",
        BrowserRequestMode::SameOrigin => "same-origin",
        BrowserRequestMode::NoCors => "no-cors",
    };
    crate::browser_http_cache::CacheKey {
        request_url: request_url.to_string(),
        partition: format!(
            "page:request-origin={request_origin}:target-origin={target_origin}:credentials={credentials}:mode={mode}"
        ),
    }
}

#[allow(clippy::too_many_arguments)]
/// Run native Fetch I/O on a worker while pumping timers and Promise
/// microtasks so compiled AOT `await` / `then` can abort an in-flight request.
fn fetch_native_interruptible(
    url: &str,
    options: FetchOptions,
    signal: &Value,
) -> Result<(FetchResponse, &'static str), String> {
    let (sender, receiver) = mpsc::channel();
    let worker_url = url.to_string();
    thread::spawn(move || {
        let result = fetch_inner(&worker_url, &options).map_err(|error| error.to_string());
        let _ = sender.send(result);
    });

    loop {
        if signal.get_property("aborted").to_bool() {
            return Err("fetch was aborted".to_string());
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(2)) {
            Ok(result) => {
                crate::jsdom::tick_timers();
                crate::jsdom::drain_microtasks();
                if signal.get_property("aborted").to_bool() {
                    return Err("fetch was aborted".to_string());
                }
                return match result {
                    Ok(response) => Ok((response, "basic")),
                    Err(error) => Ok((
                        FetchResponse {
                            status: 0,
                            ok: false,
                            status_text: error,
                            headers: HashMap::new(),
                            url: url.to_string(),
                            redirected: false,
                            body_stream: ReadableStream::from_bytes(Vec::new()),
                        },
                        "basic",
                    )),
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                crate::jsdom::tick_timers();
                crate::jsdom::drain_microtasks();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("fetch worker disconnected".to_string());
            }
        }
    }
}

fn fetch_page_bytes_interruptible(
    url: &str,
    options: FetchOptions,
    request_origin: String,
    cookies: crate::cookie_store_web::CookieSnapshot,
    credentials_mode: BrowserCredentialsMode,
    request_mode: BrowserRequestMode,
    redirect_mode: BrowserRedirectMode,
    document_url: String,
    cancellation: FetchCancellation,
    signal: &Value,
) -> Result<BrowserFetchResponse, String> {
    let (sender, receiver) = mpsc::channel();
    let url = url.to_string();
    let worker_cancellation = cancellation.clone();
    thread::spawn(move || {
        let completion_order = AtomicU64::new(0);
        let result = fetch_browser_bytes_with_redirects(
            &url,
            &options,
            &request_origin,
            &cookies,
            credentials_mode,
            request_mode == BrowserRequestMode::Cors,
            request_mode == BrowserRequestMode::Cors,
            request_mode == BrowserRequestMode::SameOrigin,
            redirect_mode,
            &document_url,
            ScriptReferrerPolicy::default(),
            &worker_cancellation,
            &completion_order,
        )
        .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });

    loop {
        if signal.get_property("aborted").to_bool() {
            cancellation.cancel();
            return Err("page Fetch was aborted".to_string());
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(2)) {
            Ok(result) => {
                // The transport timeout and AbortSignal timer can become ready
                // in the same turn. Run the page task checkpoint before
                // classifying the transport completion so the signal wins the
                // deadline race consistently.
                crate::jsdom::tick_timers();
                crate::jsdom::drain_microtasks();
                if signal.get_property("aborted").to_bool() {
                    cancellation.cancel();
                    return Err("page Fetch was aborted".to_string());
                }
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                crate::jsdom::tick_timers();
                crate::jsdom::drain_microtasks();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("page Fetch worker disconnected".to_string());
            }
        }
    }
}

fn filter_page_response_headers(
    headers: &mut HashMap<String, String>,
    cross_origin: bool,
    credentials_mode: BrowserCredentialsMode,
) {
    headers.retain(|name, _| {
        !name.eq_ignore_ascii_case("set-cookie") && !name.eq_ignore_ascii_case("set-cookie2")
    });
    if !cross_origin {
        return;
    }
    let exposed = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("access-control-expose-headers"))
        .map(|(_, value)| {
            value
                .split(',')
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let wildcard = credentials_mode != BrowserCredentialsMode::Include
        && exposed.iter().any(|name| name == "*");
    headers.retain(|name, _| {
        let name = name.to_ascii_lowercase();
        wildcard
            || matches!(
                name.as_str(),
                "cache-control"
                    | "content-language"
                    | "content-length"
                    | "content-type"
                    | "expires"
                    | "last-modified"
                    | "pragma"
            )
            || exposed.iter().any(|exposed| exposed == &name)
    });
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn method_name(method: Method) -> &'static str {
    match method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
        Method::Patch => "PATCH",
        Method::Head => "HEAD",
        Method::Options => "OPTIONS",
    }
}

fn is_cors_safelisted_method(method: Method) -> bool {
    matches!(method, Method::Get | Method::Head | Method::Post)
}

fn is_cors_safelisted_request_header(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    match name.to_ascii_lowercase().as_str() {
        "accept" | "accept-language" | "content-language" => true,
        "content-type" => matches!(
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
        ),
        "range" => {
            let value = value.trim();
            let Some(range) = value.strip_prefix("bytes=") else {
                return false;
            };
            !range.contains(',')
                && range.split_once('-').is_some_and(|(start, end)| {
                    !start.is_empty()
                        && start.chars().all(|character| character.is_ascii_digit())
                        && end.chars().all(|character| character.is_ascii_digit())
                })
        }
        _ => false,
    }
}

fn cors_unsafe_request_header_names(headers: &HashMap<String, String>) -> Vec<String> {
    let mut names = headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "origin" | "referer" | "cookie"
            )
        })
        .filter(|(name, value)| !is_cors_safelisted_request_header(name, value))
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn validate_no_cors_request(options: &FetchOptions) -> Result<(), String> {
    if !is_cors_safelisted_method(options.method) {
        return Err(format!(
            "no-cors Fetch does not allow method {}",
            method_name(options.method)
        ));
    }
    let unsafe_headers = cors_unsafe_request_header_names(&options.headers);
    if unsafe_headers.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "no-cors Fetch does not allow non-safelisted request headers: {}",
            unsafe_headers.join(", ")
        ))
    }
}

fn cors_preflight_cache_key(
    request_origin: &str,
    url: &url::Url,
    credentials_mode: BrowserCredentialsMode,
) -> CorsPreflightCacheKey {
    CorsPreflightCacheKey {
        request_origin: request_origin.to_string(),
        target_origin: url.origin().ascii_serialization(),
        credentials_mode,
    }
}

fn cors_preflight_cache_allows(
    key: &CorsPreflightCacheKey,
    method: Method,
    unsafe_headers: &[String],
) -> bool {
    {
        let mut cache = cors_preflight_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = std::time::Instant::now();
        cache.entries.retain(|_, entry| entry.expires_at > now);
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        let Some(entry) = cache.entries.get_mut(key) else {
            return false;
        };
        let method_safelisted = is_cors_safelisted_method(method);
        let method = method_name(method);
        let method_allowed =
            method_safelisted || entry.method_wildcard || entry.allowed_methods.contains(method);
        let headers_allowed = unsafe_headers.iter().all(|name| {
            entry.header_wildcard || entry.allowed_headers.contains(&name.to_ascii_lowercase())
        });
        if method_allowed && headers_allowed {
            entry.last_used = clock;
            true
        } else {
            false
        }
    }
}

fn store_cors_preflight_cache_entry(
    key: CorsPreflightCacheKey,
    allowed_methods: HashSet<String>,
    allowed_headers: HashSet<String>,
    method_wildcard: bool,
    header_wildcard: bool,
    max_age_secs: u64,
) {
    if max_age_secs == 0 {
        return;
    }
    {
        let mut cache = cors_preflight_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clock = cache.clock.wrapping_add(1);
        let last_used = cache.clock;
        cache.entries.insert(
            key,
            CorsPreflightCacheEntry {
                allowed_methods,
                allowed_headers,
                method_wildcard,
                header_wildcard,
                expires_at: std::time::Instant::now()
                    + std::time::Duration::from_secs(
                        max_age_secs.min(MAX_CORS_PREFLIGHT_CACHE_AGE_SECS),
                    ),
                last_used,
            },
        );
        while cache.entries.len() > MAX_CORS_PREFLIGHT_CACHE_ENTRIES {
            let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            cache.entries.remove(&oldest);
        }
    }
}

fn perform_cors_preflight_if_needed(
    agent: &ureq::Agent,
    url: &url::Url,
    request_origin: &str,
    credentials_mode: BrowserCredentialsMode,
    method: Method,
    request_headers: &HashMap<String, String>,
    cancellation: &FetchCancellation,
) -> Result<(), Box<dyn std::error::Error>> {
    let unsafe_headers = cors_unsafe_request_header_names(request_headers);
    if is_cors_safelisted_method(method) && unsafe_headers.is_empty() {
        return Ok(());
    }
    let cache_key = cors_preflight_cache_key(request_origin, url, credentials_mode);
    if cors_preflight_cache_allows(&cache_key, method, &unsafe_headers) {
        return Ok(());
    }
    cancellation.check()?;
    let mut headers = HashMap::from([
        ("Origin".to_string(), request_origin.to_string()),
        (
            "Access-Control-Request-Method".to_string(),
            method_name(method).to_string(),
        ),
    ]);
    if !unsafe_headers.is_empty() {
        headers.insert(
            "Access-Control-Request-Headers".to_string(),
            unsafe_headers.join(", "),
        );
    }
    let response = send_request_with_agent(agent, url.as_str(), Method::Options, &headers, None)?;
    cancellation.check()?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("CORS preflight returned HTTP {status} for {url}").into());
    }
    let response_headers = response_headers(&response);
    validate_browser_cors_headers(
        request_origin,
        url.as_str(),
        &response_headers,
        credentials_mode,
    )
    .map_err(|error| format!("CORS preflight origin check failed for {url}: {error}"))?;
    let header = |name: &str| {
        response_headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    };
    let allowed_methods = header("access-control-allow-methods")
        .map(|value| {
            value
                .split(',')
                .map(|method| method.trim().to_ascii_uppercase())
                .filter(|method| !method.is_empty() && method != "*")
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let method_wildcard = credentials_mode != BrowserCredentialsMode::Include
        && header("access-control-allow-methods")
            .is_some_and(|value| value.split(',').any(|method| method.trim() == "*"));
    if !is_cors_safelisted_method(method)
        && !method_wildcard
        && !allowed_methods.contains(method_name(method))
    {
        return Err(format!(
            "CORS preflight did not allow method {} for {url}",
            method_name(method)
        )
        .into());
    }
    let allowed_headers = header("access-control-allow-headers")
        .map(|value| {
            value
                .split(',')
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty() && name != "*")
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let header_wildcard = credentials_mode != BrowserCredentialsMode::Include
        && header("access-control-allow-headers")
            .is_some_and(|value| value.split(',').any(|name| name.trim() == "*"));
    if !unsafe_headers.is_empty() {
        let missing = unsafe_headers
            .iter()
            .filter(|name| !header_wildcard && !allowed_headers.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "CORS preflight did not allow request headers for {url}: {}",
                missing.join(", ")
            )
            .into());
        }
    }
    let max_age_secs = header("access-control-max-age")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(5);
    store_cors_preflight_cache_entry(
        cache_key,
        allowed_methods,
        allowed_headers,
        method_wildcard,
        header_wildcard,
        max_age_secs,
    );
    Ok(())
}

type HeaderList = Rc<RefCell<Vec<(String, String)>>>;

fn normalized_header_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn normalized_header_value(value: &str) -> String {
    value.trim().to_string()
}

fn header_set(list: &HeaderList, name: &str, value: &str) {
    let name = normalized_header_name(name);
    let value = normalized_header_value(value);
    let mut headers = list.borrow_mut();
    headers.retain(|(candidate, _)| candidate != &name);
    headers.push((name, value));
}

fn header_append(list: &HeaderList, name: &str, value: &str) {
    let name = normalized_header_name(name);
    let value = normalized_header_value(value);
    let mut headers = list.borrow_mut();
    if let Some((_, current)) = headers.iter_mut().find(|(candidate, _)| candidate == &name) {
        if !current.is_empty() {
            current.push_str(", ");
        }
        current.push_str(&value);
    } else {
        headers.push((name, value));
    }
}

fn collect_header_init(init: &Value) -> HeaderList {
    let list = Rc::new(RefCell::new(Vec::new()));
    if init.is_nullish() {
        return list;
    }
    let for_each = init.get_property("forEach");
    if for_each.is_function() {
        let collected = Rc::clone(&list);
        init.call_method(
            "forEach",
            vec![realm_fetch_function(move |_, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                let name = args.get(1).cloned().unwrap_or(Value::Undefined);
                header_append(&collected, &name.to_js_string(), &value.to_js_string());
                Value::Undefined
            })],
        );
        return list;
    }
    if let Value::Array(entries) = init {
        for entry in entries.borrow().iter() {
            header_append(
                &list,
                &entry.get_property("0").to_js_string(),
                &entry.get_property("1").to_js_string(),
            );
        }
        return list;
    }
    if let Value::Object(object) = init {
        let object = object.borrow();
        for name in object.keys() {
            header_append(&list, &name, &object.get_direct(&name).to_js_string());
        }
    }
    list
}

fn headers_value_from_list(list: HeaderList) -> Value {
    let mut props = HashMap::new();
    let append_list = Rc::clone(&list);
    props.insert(
        "append".to_string(),
        realm_fetch_function(move |_, args| {
            header_append(
                &append_list,
                &args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
                &args
                    .get(1)
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
            );
            Value::Undefined
        }),
    );
    let delete_list = Rc::clone(&list);
    props.insert(
        "delete".to_string(),
        realm_fetch_function(move |_, args| {
            let name = normalized_header_name(
                &args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
            );
            delete_list
                .borrow_mut()
                .retain(|(candidate, _)| candidate != &name);
            Value::Undefined
        }),
    );
    let get_list = Rc::clone(&list);
    props.insert(
        "get".to_string(),
        realm_fetch_function(move |_, args| {
            let name = normalized_header_name(
                &args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
            );
            get_list
                .borrow()
                .iter()
                .find(|(candidate, _)| candidate == &name)
                .map(|(_, value)| Value::from(value.clone()))
                .unwrap_or(Value::Null)
        }),
    );
    let has_list = Rc::clone(&list);
    props.insert(
        "has".to_string(),
        realm_fetch_function(move |_, args| {
            let name = normalized_header_name(
                &args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
            );
            Value::Bool(
                has_list
                    .borrow()
                    .iter()
                    .any(|(candidate, _)| candidate == &name),
            )
        }),
    );
    let set_list = Rc::clone(&list);
    props.insert(
        "set".to_string(),
        realm_fetch_function(move |_, args| {
            header_set(
                &set_list,
                &args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
                &args
                    .get(1)
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
            );
            Value::Undefined
        }),
    );
    let for_each_list = Rc::clone(&list);
    props.insert(
        "forEach".to_string(),
        realm_fetch_function(move |this, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            for (name, value) in for_each_list.borrow().iter() {
                callback.call(
                    Value::Undefined,
                    vec![
                        Value::from(value.clone()),
                        Value::from(name.clone()),
                        this.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    for method in ["entries", "keys", "values"] {
        let snapshot = Rc::clone(&list);
        props.insert(
            method.to_string(),
            realm_fetch_function(move |_, _| {
                let values = snapshot
                    .borrow()
                    .iter()
                    .map(|(name, value)| match method {
                        "keys" => Value::from(name.clone()),
                        "values" => Value::from(value.clone()),
                        _ => Value::array(vec![
                            Value::from(name.clone()),
                            Value::from(value.clone()),
                        ]),
                    })
                    .collect();
                Value::array(values)
            }),
        );
    }
    Value::object(props)
}

pub fn headers_class() -> Value {
    HEADERS_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_fetch_function(|_, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            headers_value_from_list(collect_header_init(&init))
        });
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        for method in [
            "append",
            "delete",
            "entries",
            "forEach",
            "get",
            "getSetCookie",
            "has",
            "keys",
            "set",
            "values",
        ] {
            prototype.set_property(method, realm_fetch_function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn headers_to_map(headers: &Value) -> HashMap<String, String> {
    collect_header_init(headers)
        .borrow()
        .iter()
        .cloned()
        .collect()
}

fn response_from_parts(
    body: String,
    status: u16,
    status_text: String,
    headers: Value,
    url: String,
    response_type: String,
) -> Value {
    response_from_bytes(
        body.into_bytes(),
        status,
        status_text,
        headers,
        url,
        response_type,
    )
}

fn response_from_bytes(
    body: Vec<u8>,
    status: u16,
    status_text: String,
    headers: Value,
    url: String,
    response_type: String,
) -> Value {
    let body_used = Rc::new(Cell::new(false));
    let body_used_for_stream = Rc::clone(&body_used);
    let body_stream = crate::streams_web::from_bytes(
        body.clone(),
        realm_fetch_function(move |_, _| {
            body_used_for_stream.set(true);
            Value::Undefined
        }),
    );
    let ok = (200..300).contains(&status);
    let mut props = HashMap::from([
        ("status".into(), Value::Number(status as f64)),
        ("ok".into(), Value::Bool(ok)),
        ("statusText".into(), Value::from(status_text.clone())),
        ("headers".into(), headers.clone()),
        ("url".into(), Value::from(url.clone())),
        ("type".into(), Value::from(response_type.clone())),
        ("redirected".into(), Value::Bool(false)),
        ("body".into(), body_stream),
    ]);
    let body_used_getter = Rc::clone(&body_used);
    props.insert(
        "__w3cos_getter_bodyUsed".into(),
        realm_fetch_function(move |_, _| Value::Bool(body_used_getter.get())),
    );
    let text_body = String::from_utf8_lossy(&body).into_owned();
    let text_used = Rc::clone(&body_used);
    props.insert(
        "text".into(),
        realm_fetch_function(move |_, _| {
            text_used.set(true);
            Value::from(text_body.clone())
        }),
    );
    let json_body = String::from_utf8_lossy(&body).into_owned();
    let json_used = Rc::clone(&body_used);
    props.insert(
        "json".into(),
        realm_fetch_function(move |_, _| {
            json_used.set(true);
            w3cos_core::json::parse(vec![Value::from(json_body.clone())])
        }),
    );
    let bytes = body.clone();
    let bytes_used = Rc::clone(&body_used);
    props.insert(
        "arrayBuffer".into(),
        realm_fetch_function(move |_, _| {
            bytes_used.set(true);
            w3cos_core::binary::array_buffer_value(bytes.clone())
        }),
    );
    let clone_body = body;
    props.insert(
        "clone".into(),
        realm_fetch_function(move |_, _| {
            response_from_bytes(
                clone_body.clone(),
                status,
                status_text.clone(),
                headers_value_from_list(collect_header_init(&headers)),
                url.clone(),
                response_type.clone(),
            )
        }),
    );
    Value::object(props)
}

fn response_from_native_stream(
    body: ReadableStream,
    status: u16,
    status_text: String,
    headers: Value,
    url: String,
    response_type: String,
) -> Value {
    let body_used = Rc::new(Cell::new(false));
    let body_used_for_stream = Rc::clone(&body_used);
    let body_stream = crate::streams_web::from_native_stream(
        body,
        realm_fetch_function(move |_, _| {
            body_used_for_stream.set(true);
            Value::Undefined
        }),
    );
    let mut props = HashMap::from([
        ("status".into(), Value::Number(status as f64)),
        ("ok".into(), Value::Bool((200..300).contains(&status))),
        ("statusText".into(), Value::from(status_text)),
        ("headers".into(), headers),
        ("url".into(), Value::from(url)),
        ("type".into(), Value::from(response_type)),
        ("redirected".into(), Value::Bool(false)),
        ("body".into(), body_stream),
    ]);
    let body_used_getter = Rc::clone(&body_used);
    props.insert(
        "__w3cos_getter_bodyUsed".into(),
        realm_fetch_function(move |_, _| Value::Bool(body_used_getter.get())),
    );
    for method in ["text", "json", "arrayBuffer", "clone"] {
        props.insert(
            method.into(),
            realm_fetch_function(move |_, _| {
                w3cos_core::promise::reject(vec![Value::object(HashMap::from([
                    ("name".into(), Value::string("TypeError")),
                    (
                        "message".into(),
                        Value::from(format!(
                            "Response.{method}() is unavailable after selecting the streaming body"
                        )),
                    ),
                ]))])
            }),
        );
    }
    Value::object(props)
}

fn response_value(response: FetchResponse, _requested_url: String) -> Value {
    let is_event_stream = response
        .header("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    let status = response.status;
    let status_text = response.status_text.clone();
    let url = response.url.clone();
    let redirected = response.redirected;
    let headers = headers_value_from_list(Rc::new(RefCell::new(
        response
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )));
    let value = if is_event_stream {
        response_from_native_stream(
            response.body_stream,
            status,
            status_text,
            headers,
            url,
            "basic".into(),
        )
    } else {
        let body = response.array_buffer().unwrap_or_default();
        response_from_bytes(body, status, status_text, headers, url, "basic".into())
    };
    value.set_property("redirected", Value::Bool(redirected));
    value
}

fn fetch_error_value(url: &str, name: &str, message: &str) -> Value {
    response_from_parts(
        String::new(),
        0,
        format!("{name}: {message}"),
        headers_value_from_list(Rc::new(RefCell::new(Vec::new()))),
        url.to_string(),
        "error".into(),
    )
}

/// Thenable Response facade used by both Promise `then` and property access.
///
/// The facade copies the Response's own properties and forwards Promise
/// methods to a resolved promise containing the untouched Response.
/// Keeping the fulfilled value separate prevents Promise resolution from
/// recursively assimilating the facade's own `then` method.
fn fetch_promise_facade(response: Value) -> Value {
    let properties = match &response {
        Value::Object(object) => {
            let object = object.borrow();
            let keys = match object.own_keys() {
                Value::Array(keys) => keys.borrow().clone(),
                _ => Vec::new(),
            };
            keys.into_iter()
                .map(|key| {
                    let key = key.to_js_string();
                    let value = object.get_direct(&key);
                    (key, value)
                })
                .collect()
        }
        _ => HashMap::new(),
    };
    let facade = Value::object(properties);
    let promise = w3cos_core::promise::resolve(vec![response]);
    for method in ["then", "catch", "finally"] {
        let promise = promise.clone();
        facade.set_property(
            method,
            Value::function(move |_, args| promise.call_method(method, args)),
        );
    }
    facade
}

pub fn response_class() -> Value {
    RESPONSE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let constructor = realm_fetch_function(|_, args| {
            let body = match args.first() {
                None | Some(Value::Undefined) | Some(Value::Null) => String::new(),
                Some(body) => body.to_js_string(),
            };
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            let status = if init.get_property("status").is_number() {
                init.get_property("status").to_u32() as u16
            } else {
                200
            };
            let status_text = match init.get_property("statusText") {
                Value::Undefined => String::new(),
                value => value.to_js_string(),
            };
            let headers =
                headers_value_from_list(collect_header_init(&init.get_property("headers")));
            response_from_parts(
                body,
                status,
                status_text,
                headers,
                String::new(),
                "default".into(),
            )
        });
        constructor.set_property(
            "error",
            realm_fetch_function(|_, _| {
                response_from_parts(
                    String::new(),
                    0,
                    String::new(),
                    headers_value_from_list(Rc::new(RefCell::new(Vec::new()))),
                    String::new(),
                    "error".into(),
                )
            }),
        );
        constructor.set_property(
            "redirect",
            realm_fetch_function(|_, args| {
                let url = args.first().cloned().unwrap_or(Value::Undefined);
                let status = args.get(1).map(Value::to_u32).unwrap_or(302) as u16;
                response_from_parts(
                    String::new(),
                    status,
                    String::new(),
                    headers_value_from_list(Rc::new(RefCell::new(vec![(
                        "location".into(),
                        url.to_js_string(),
                    )]))),
                    String::new(),
                    "default".into(),
                )
            }),
        );
        constructor.set_property(
            "json",
            realm_fetch_function(|_, args| {
                let data = args.first().cloned().unwrap_or(Value::Null);
                let init = args.get(1).cloned().unwrap_or(Value::Undefined);
                let status = if init.get_property("status").is_number() {
                    init.get_property("status").to_u32() as u16
                } else {
                    200
                };
                let headers =
                    headers_value_from_list(collect_header_init(&init.get_property("headers")));
                if !headers
                    .call_method("has", vec![Value::from("content-type")])
                    .to_bool()
                {
                    headers.call_method(
                        "set",
                        vec![Value::from("content-type"), Value::from("application/json")],
                    );
                }
                response_from_parts(
                    w3cos_core::json::stringify(vec![data]).to_js_string(),
                    status,
                    String::new(),
                    headers,
                    String::new(),
                    "default".into(),
                )
            }),
        );
        let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
        for member in [
            "arrayBuffer",
            "blob",
            "body",
            "bodyUsed",
            "bytes",
            "clone",
            "formData",
            "headers",
            "json",
            "ok",
            "redirected",
            "status",
            "statusText",
            "text",
            "type",
            "url",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        constructor.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(constructor.clone());
        constructor
    })
}

fn request_value(input: Value, init: Value) -> Value {
    let inherited = !input.get_property("url").is_undefined();
    let url = if inherited {
        input.get_property("url").to_js_string()
    } else {
        input.to_js_string()
    };
    let inherited_method = if inherited {
        input.get_property("method")
    } else {
        Value::from("GET")
    };
    let method = if init.get_property("method").is_undefined() {
        inherited_method.to_js_string().to_uppercase()
    } else {
        init.get_property("method").to_js_string().to_uppercase()
    };
    let inherited_headers = if inherited {
        input.get_property("headers")
    } else {
        Value::Undefined
    };
    let headers_init = if init.get_property("headers").is_undefined() {
        inherited_headers
    } else {
        init.get_property("headers")
    };
    let headers = headers_value_from_list(collect_header_init(&headers_init));
    let inherited_body = if inherited {
        input.get_property("__w3cos_body")
    } else {
        Value::Undefined
    };
    let body = if init.get_property("body").is_undefined() {
        inherited_body
    } else {
        init.get_property("body")
    };
    if let Some((_, content_type)) = crate::form_data::serialize(&body) {
        if !headers
            .call_method("has", vec![Value::string("content-type")])
            .to_bool()
        {
            headers.call_method(
                "set",
                vec![Value::string("content-type"), Value::string(&content_type)],
            );
        }
    }
    let signal = if init.get_property("signal").is_undefined() && inherited {
        input.get_property("signal")
    } else {
        init.get_property("signal")
    };
    let inherited_cache = if inherited {
        input.get_property("cache")
    } else {
        Value::Undefined
    };
    let cache = if init.get_property("cache").is_undefined() {
        if inherited_cache.is_undefined() {
            Value::from("default")
        } else {
            inherited_cache
        }
    } else {
        init.get_property("cache")
    };
    let inherited_credentials = if inherited {
        input.get_property("credentials")
    } else {
        Value::Undefined
    };
    let credentials = if init.get_property("credentials").is_undefined() {
        if inherited_credentials.is_undefined() {
            Value::from("same-origin")
        } else {
            inherited_credentials
        }
    } else {
        init.get_property("credentials")
    };
    let inherited_mode = if inherited {
        input.get_property("mode")
    } else {
        Value::Undefined
    };
    let mode = if init.get_property("mode").is_undefined() {
        if inherited_mode.is_undefined() {
            Value::from("cors")
        } else {
            inherited_mode
        }
    } else {
        init.get_property("mode")
    };
    let inherited_redirect = if inherited {
        input.get_property("redirect")
    } else {
        Value::Undefined
    };
    let redirect = if init.get_property("redirect").is_undefined() {
        if inherited_redirect.is_undefined() {
            Value::from("follow")
        } else {
            inherited_redirect
        }
    } else {
        init.get_property("redirect")
    };
    let mut props = HashMap::from([
        ("url".into(), Value::from(url.clone())),
        ("method".into(), Value::from(method.clone())),
        ("headers".into(), headers.clone()),
        ("signal".into(), signal.clone()),
        ("body".into(), Value::Null),
        ("bodyUsed".into(), Value::Bool(false)),
        ("cache".into(), cache),
        ("credentials".into(), credentials),
        ("destination".into(), Value::from("")),
        ("integrity".into(), Value::from("")),
        ("mode".into(), mode),
        ("redirect".into(), redirect),
        ("referrer".into(), Value::from("about:client")),
        ("referrerPolicy".into(), Value::from("")),
        ("__w3cos_body".into(), body.clone()),
    ]);
    let clone_input = Value::object(props.clone());
    props.insert(
        "clone".into(),
        realm_fetch_function(move |_, _| request_value(clone_input.clone(), Value::Undefined)),
    );
    let text_body = body.clone();
    props.insert(
        "text".into(),
        realm_fetch_function(move |_, _| {
            if let Some((body, _)) = crate::form_data::serialize(&text_body) {
                return Value::string(&body);
            }
            match &text_body {
                Value::Undefined | Value::Null => Value::from(""),
                value => Value::from(value.to_js_string()),
            }
        }),
    );
    let json_body = body;
    props.insert(
        "json".into(),
        realm_fetch_function(move |_, _| {
            w3cos_core::json::parse(vec![Value::from(json_body.to_js_string())])
        }),
    );
    Value::object(props)
}

pub fn request_class() -> Value {
    REQUEST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_fetch_function(|_, args| {
            request_value(
                args.first().cloned().unwrap_or(Value::Undefined),
                args.get(1).cloned().unwrap_or(Value::Undefined),
            )
        });
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        for member in [
            "arrayBuffer",
            "blob",
            "body",
            "bodyUsed",
            "bytes",
            "cache",
            "clone",
            "credentials",
            "destination",
            "duplex",
            "formData",
            "headers",
            "integrity",
            "isHistoryNavigation",
            "isReloadNavigation",
            "json",
            "keepalive",
            "method",
            "mode",
            "redirect",
            "referrer",
            "referrerPolicy",
            "signal",
            "targetAddressSpace",
            "text",
            "url",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

struct AbortState {
    aborted: Cell<bool>,
    reason: RefCell<Value>,
    listeners: RefCell<Vec<Value>>,
}

struct AbortBinding {
    signal: Value,
    state: Rc<AbortState>,
}

thread_local! {
    static ABORT_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ABORT_SIGNAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ABORT_BINDINGS: RefCell<Vec<AbortBinding>> = const { RefCell::new(Vec::new()) };
}

fn new_abort_state() -> Rc<AbortState> {
    Rc::new(AbortState {
        aborted: Cell::new(false),
        reason: RefCell::new(Value::Undefined),
        listeners: RefCell::new(Vec::new()),
    })
}

fn abort_signal_value(state: Rc<AbortState>) -> Value {
    let mut props = HashMap::from([("onabort".into(), Value::Null)]);
    let aborted_state = Rc::clone(&state);
    props.insert(
        "__w3cos_getter_aborted".into(),
        realm_fetch_function(move |_, _| Value::Bool(aborted_state.aborted.get())),
    );
    let reason_state = Rc::clone(&state);
    props.insert(
        "__w3cos_getter_reason".into(),
        realm_fetch_function(move |_, _| reason_state.reason.borrow().clone()),
    );
    let add_state = Rc::clone(&state);
    props.insert(
        "addEventListener".into(),
        realm_fetch_function(move |_, args| {
            if args.first().map(Value::to_js_string).as_deref() == Some("abort") {
                let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
                if listener.is_function() {
                    add_state.listeners.borrow_mut().push(listener);
                }
            }
            Value::Undefined
        }),
    );
    let remove_state = Rc::clone(&state);
    props.insert(
        "removeEventListener".into(),
        realm_fetch_function(move |_, args| {
            let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
            remove_state
                .listeners
                .borrow_mut()
                .retain(|candidate| !candidate.same_value_zero(&listener));
            Value::Undefined
        }),
    );
    let throw_state = Rc::clone(&state);
    props.insert(
        "throwIfAborted".into(),
        realm_fetch_function(move |_, _| {
            if throw_state.aborted.get() {
                w3cos_core::throw_value(throw_state.reason.borrow().clone());
            }
            Value::Undefined
        }),
    );
    let native_abort_state = Rc::clone(&state);
    props.insert(
        "__w3cos_abort_native".into(),
        realm_fetch_function(move |signal, args| {
            abort_state(
                &native_abort_state,
                &signal,
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::from("AbortError")),
            );
            Value::Undefined
        }),
    );
    let signal = Value::object(props);
    w3cos_core::class::set_prototype_of(&signal, &abort_signal_class().get_property("prototype"));
    ABORT_BINDINGS.with(|bindings| {
        bindings.borrow_mut().push(AbortBinding {
            signal: signal.clone(),
            state,
        });
    });
    signal
}

fn abort_state(state: &Rc<AbortState>, signal: &Value, reason: Value) {
    if state.aborted.replace(true) {
        return;
    }
    *state.reason.borrow_mut() = reason.clone();
    let event = Value::object(HashMap::from([
        ("type".into(), Value::from("abort")),
        ("target".into(), signal.clone()),
        ("currentTarget".into(), signal.clone()),
    ]));
    let onabort = signal.get_property("onabort");
    if onabort.is_function() {
        onabort.call(signal.clone(), vec![event.clone()]);
    }
    for listener in state.listeners.borrow().clone() {
        listener.call(signal.clone(), vec![event.clone()]);
    }
}

pub fn abort_controller_class() -> Value {
    ABORT_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_fetch_function(|_, _| {
            let state = new_abort_state();
            let signal = abort_signal_value(Rc::clone(&state));
            let signal_for_abort = signal.clone();
            let controller = Value::object(HashMap::from([
                ("signal".into(), signal),
                (
                    "abort".into(),
                    realm_fetch_function(move |_, args| {
                        let reason = args
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Value::from("AbortError"));
                        abort_state(&state, &signal_for_abort, reason);
                        Value::Undefined
                    }),
                ),
            ]));
            w3cos_core::class::set_prototype_of(
                &controller,
                &abort_controller_class().get_property("prototype"),
            );
            controller
        });
        class.set_property("name", Value::string("AbortController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("abort", realm_fetch_function(|_, _| Value::Undefined));
        prototype.set_property("signal", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn abort_signal_class() -> Value {
    ABORT_SIGNAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_fetch_function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: AbortSignal")],
            ))
        });
        class.set_property("name", Value::string("AbortSignal"));
        class.set_property(
            "abort",
            realm_fetch_function(|_, args| {
                let state = new_abort_state();
                let signal = abort_signal_value(Rc::clone(&state));
                abort_state(
                    &state,
                    &signal,
                    args.first()
                        .cloned()
                        .unwrap_or_else(|| Value::from("AbortError")),
                );
                signal
            }),
        );
        class.set_property(
            "timeout",
            realm_fetch_function(|_, args| {
                let timeout_ms = args.first().map(Value::to_number).unwrap_or(0.0).max(0.0);
                let state = new_abort_state();
                let signal = abort_signal_value(state);
                signal.set_property("__w3cos_timeout_ms", Value::Number(timeout_ms));
                let timeout_signal = signal.clone();
                crate::jsdom::schedule_timeout_value(
                    realm_fetch_function(move |_, _| {
                        timeout_signal
                            .call_method("__w3cos_abort_native", vec![Value::from("TimeoutError")]);
                        Value::Undefined
                    }),
                    timeout_ms as u64,
                );
                signal
            }),
        );
        class.set_property(
            "any",
            realm_fetch_function(|_, args| {
                let state = new_abort_state();
                let signal = abort_signal_value(Rc::clone(&state));
                let sources = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Array(sources) = sources {
                    for source in sources.borrow().iter().cloned() {
                        if source.get_property("aborted").to_bool() {
                            abort_state(&state, &signal, source.get_property("reason"));
                            break;
                        }
                        let aggregate_state = Rc::clone(&state);
                        let aggregate_signal = signal.clone();
                        let source_for_reason = source.clone();
                        source.call_method(
                            "addEventListener",
                            vec![
                                Value::from("abort"),
                                realm_fetch_function(move |_, _| {
                                    abort_state(
                                        &aggregate_state,
                                        &aggregate_signal,
                                        source_for_reason.get_property("reason"),
                                    );
                                    Value::Undefined
                                }),
                            ],
                        );
                    }
                }
                signal
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["aborted", "onabort", "reason"] {
            prototype.set_property(property, Value::Undefined);
        }
        prototype.set_property(
            "throwIfAborted",
            realm_fetch_function(|_, _| Value::Undefined),
        );
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub(crate) fn reset_realm() {
    let bindings = ABORT_BINDINGS.with(|bindings| std::mem::take(&mut *bindings.borrow_mut()));
    for binding in bindings {
        binding.state.aborted.set(true);
        *binding.state.reason.borrow_mut() = Value::Undefined;
        binding.state.listeners.borrow_mut().clear();
        binding.signal.set_property("onabort", Value::Null);
        for method in [
            "__w3cos_getter_aborted",
            "__w3cos_getter_reason",
            "__w3cos_abort_native",
            "addEventListener",
            "removeEventListener",
            "throwIfAborted",
        ] {
            binding.signal.set_property(method, Value::Undefined);
        }
    }
    for slot in [
        &HEADERS_CLASS,
        &RESPONSE_CLASS,
        &REQUEST_CLASS,
        &ABORT_CONTROLLER_CLASS,
        &ABORT_SIGNAL_CLASS,
    ] {
        slot.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn build_agent(options: &FetchOptions) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        // Browser fetch has no implicit deadline. Callers that need one use
        // RequestInit.timeout or AbortSignal.timeout explicitly.
        .timeout_global(options.timeout_ms.map(std::time::Duration::from_millis))
        .save_redirect_history(true)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

fn fetch_inner(
    url: &str,
    options: &FetchOptions,
) -> Result<FetchResponse, Box<dyn std::error::Error>> {
    use ureq::ResponseExt as _;

    let resp = send_request(url, options)?;
    let status = resp.status().as_u16();
    let status_text = resp
        .status()
        .canonical_reason()
        .unwrap_or("Unknown")
        .to_string();
    let final_url = resp.get_uri().to_string();
    let redirected = resp
        .get_redirect_history()
        .is_some_and(|history| history.len() > 1);
    let headers = response_headers(&resp);

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
        url: final_url,
        redirected,
        body_stream,
    })
}

fn send_request(
    url: &str,
    options: &FetchOptions,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let agent = build_agent(options);
    send_request_with_agent(
        &agent,
        url,
        options.method,
        &options.headers,
        options.body.as_deref(),
    )
}

fn send_request_with_agent(
    agent: &ureq::Agent,
    url: &str,
    method: Method,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let without_body = |mut request: ureq::RequestBuilder<ureq::typestate::WithoutBody>| {
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        request.call()
    };
    let with_body = |mut request: ureq::RequestBuilder<ureq::typestate::WithBody>| {
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(body) = body {
            if !headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("content-type"))
            {
                request = request.header("Content-Type", "application/json");
            }
            request.send(body.as_bytes())
        } else {
            request.send_empty()
        }
    };
    match method {
        Method::Get => without_body(agent.get(url)),
        Method::Post => with_body(agent.post(url)),
        Method::Put => with_body(agent.put(url)),
        Method::Delete => without_body(agent.delete(url)),
        Method::Patch => with_body(agent.patch(url)),
        Method::Head => without_body(agent.head(url)),
        Method::Options => without_body(agent.options(url)),
    }
}

pub enum FetchResult {
    Success(FetchResponse),
    Error(String),
}

#[derive(Debug)]
pub struct FetchTextResponse {
    pub status: u16,
    pub ok: bool,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub url: String,
    pub redirected: bool,
    /// Every credential-eligible `Set-Cookie` observed by the script transport,
    /// paired with the URL that produced it.
    pub set_cookies: Vec<(String, String)>,
    pub body: String,
}

#[cfg(feature = "dynamic-js")]
#[derive(Debug)]
pub(crate) struct FetchBinaryResponse {
    pub status: u16,
    pub ok: bool,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub url: String,
    pub redirected: bool,
    pub set_cookies: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
struct BrowserFetchResponse {
    status: u16,
    ok: bool,
    status_text: String,
    headers: HashMap<String, String>,
    url: String,
    redirected: bool,
    set_cookies: Vec<(String, String)>,
    body: Vec<u8>,
}

impl BrowserFetchResponse {
    fn from_cached(cached: crate::browser_http_cache::CachedResponse) -> Self {
        Self {
            status: cached.status,
            ok: (200..300).contains(&cached.status),
            status_text: cached.status_text,
            headers: cached.headers,
            url: cached.final_url,
            redirected: false,
            set_cookies: Vec::new(),
            body: cached.body,
        }
    }

    fn into_text(self) -> Result<FetchTextResponse, String> {
        let body = String::from_utf8(self.body).map_err(|error| error.to_string())?;
        Ok(FetchTextResponse {
            status: self.status,
            ok: self.ok,
            status_text: self.status_text,
            headers: self.headers,
            url: self.url,
            redirected: self.redirected,
            set_cookies: self.set_cookies,
            body,
        })
    }
}

#[cfg(feature = "dynamic-js")]
#[derive(Debug)]
pub(crate) struct FetchBytesResponse {
    pub status: u16,
    pub ok: bool,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub url: String,
    pub redirected: bool,
    pub set_cookies: Vec<(String, String)>,
}

#[cfg(feature = "dynamic-js")]
#[derive(Debug)]
pub(crate) enum DocumentFetchEvent {
    Response(FetchBytesResponse),
    BodyChunk(Vec<u8>),
    Complete,
    Error(String),
}

const SCRIPT_FETCH_CANCELLED: &str = "script fetch was cancelled";
static NEXT_TEXT_FETCH_COMPLETION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Default)]
struct FetchCancellation {
    cancelled: Arc<AtomicBool>,
}

impl FetchCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err(SCRIPT_FETCH_CANCELLED.to_string())
        } else {
            Ok(())
        }
    }
}

pub(crate) struct TextFetchTask {
    pub(crate) receiver: mpsc::Receiver<Result<FetchTextResponse, String>>,
    cancellation: FetchCancellation,
    completion_order: Arc<AtomicU64>,
}

#[cfg(feature = "dynamic-js")]
pub(crate) struct BinaryFetchTask {
    pub(crate) receiver: mpsc::Receiver<Result<FetchBinaryResponse, String>>,
    cancellation: FetchCancellation,
}

#[cfg(feature = "dynamic-js")]
pub(crate) struct BytesFetchTask {
    pub(crate) receiver: mpsc::Receiver<DocumentFetchEvent>,
    cancellation: FetchCancellation,
}

#[cfg(feature = "dynamic-js")]
impl BytesFetchTask {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl TextFetchTask {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub(crate) fn completion_order(&self) -> u64 {
        self.completion_order.load(Ordering::Acquire)
    }
}

#[cfg(feature = "dynamic-js")]
impl BinaryFetchTask {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }
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

/// Non-blocking buffered-text fetch for host loaders.
///
/// Unlike [`fetch_async`], the response body is fully consumed on the worker
/// thread. Embedders can therefore poll the receiver from their event loop
/// without accidentally blocking that loop while reading the body.
pub fn fetch_text_async(
    url: &str,
    options: FetchOptions,
) -> mpsc::Receiver<Result<FetchTextResponse, String>> {
    fetch_text_async_cancellable(url, options).receiver
}

pub(crate) fn fetch_text_async_cancellable(url: &str, options: FetchOptions) -> TextFetchTask {
    let (tx, rx) = mpsc::channel();
    let url = url.to_string();
    let cancellation = FetchCancellation::default();
    let worker_cancellation = cancellation.clone();
    let completion_order = Arc::new(AtomicU64::new(0));
    let worker_completion_order = Arc::clone(&completion_order);

    thread::spawn(move || {
        let result = fetch_text_with_cancellation(
            &url,
            &options,
            &worker_cancellation,
            &worker_completion_order,
        );
        mark_text_fetch_completion(&worker_completion_order);
        let _ = tx.send(result);
    });

    TextFetchTask {
        receiver: rx,
        cancellation,
        completion_order,
    }
}

#[cfg(feature = "dynamic-js")]
pub(crate) fn fetch_script_text_async(
    url: &str,
    options: FetchOptions,
    request_origin: String,
    cookies: crate::cookie_store_web::CookieSnapshot,
    credentials_mode: crate::dynamic_script::ModuleCredentialsMode,
    cors_mode: bool,
    referrer_source: String,
    referrer_policy: ScriptReferrerPolicy,
) -> TextFetchTask {
    let (tx, rx) = mpsc::channel();
    let url = url.to_string();
    let cancellation = FetchCancellation::default();
    let worker_cancellation = cancellation.clone();
    let completion_order = Arc::new(AtomicU64::new(0));
    let worker_completion_order = Arc::clone(&completion_order);
    thread::spawn(move || {
        let credentials_mode = match credentials_mode {
            crate::dynamic_script::ModuleCredentialsMode::Omit => BrowserCredentialsMode::Omit,
            crate::dynamic_script::ModuleCredentialsMode::SameOrigin => {
                BrowserCredentialsMode::SameOrigin
            }
            crate::dynamic_script::ModuleCredentialsMode::Include => {
                BrowserCredentialsMode::Include
            }
        };
        let result = fetch_browser_text_with_redirects(
            &url,
            &options,
            &request_origin,
            &cookies,
            credentials_mode,
            cors_mode,
            false,
            false,
            BrowserRedirectMode::Follow,
            &referrer_source,
            referrer_policy,
            &worker_cancellation,
            &worker_completion_order,
        )
        .map_err(|error| error.to_string());
        mark_text_fetch_completion(&worker_completion_order);
        let _ = tx.send(result);
    });
    TextFetchTask {
        receiver: rx,
        cancellation,
        completion_order,
    }
}

#[cfg(feature = "dynamic-js")]
pub(crate) fn fetch_script_bytes_async(
    url: &str,
    options: FetchOptions,
    request_origin: String,
    cookies: crate::cookie_store_web::CookieSnapshot,
    credentials_mode: crate::dynamic_script::ModuleCredentialsMode,
    cors_mode: bool,
    referrer_source: String,
    referrer_policy: ScriptReferrerPolicy,
) -> BinaryFetchTask {
    let (tx, rx) = mpsc::channel();
    let url = url.to_string();
    let cancellation = FetchCancellation::default();
    let worker_cancellation = cancellation.clone();
    thread::spawn(move || {
        let credentials_mode = match credentials_mode {
            crate::dynamic_script::ModuleCredentialsMode::Omit => BrowserCredentialsMode::Omit,
            crate::dynamic_script::ModuleCredentialsMode::SameOrigin => {
                BrowserCredentialsMode::SameOrigin
            }
            crate::dynamic_script::ModuleCredentialsMode::Include => {
                BrowserCredentialsMode::Include
            }
        };
        let completion_order = AtomicU64::new(0);
        let result = fetch_browser_bytes_with_redirects(
            &url,
            &options,
            &request_origin,
            &cookies,
            credentials_mode,
            cors_mode,
            false,
            false,
            BrowserRedirectMode::Follow,
            &referrer_source,
            referrer_policy,
            &worker_cancellation,
            &completion_order,
        )
        .map(|response| FetchBinaryResponse {
            status: response.status,
            ok: response.ok,
            status_text: response.status_text,
            headers: response.headers,
            url: response.url,
            redirected: response.redirected,
            set_cookies: response.set_cookies,
            body: response.body,
        })
        .map_err(|error| error.to_string());
        let _ = tx.send(result);
    });
    BinaryFetchTask {
        receiver: rx,
        cancellation,
    }
}

/// Materialize the initial request headers used by the shared Browser
/// subresource transport. Cache lookups must hash the same Cookie/Origin/
/// Referer and author headers that the worker will actually send; keeping this
/// beside `browser_request_headers` prevents loaders from growing a parallel
/// request-header implementation.
#[cfg(feature = "dynamic-js")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn browser_subresource_request_headers(
    url: &str,
    options: &FetchOptions,
    request_origin: &str,
    cookies: &crate::cookie_store_web::CookieSnapshot,
    credentials_mode: crate::dynamic_script::ModuleCredentialsMode,
    cors_mode: bool,
    referrer_source: &str,
    referrer_policy: ScriptReferrerPolicy,
) -> Result<HashMap<String, String>, String> {
    let current =
        url::Url::parse(url).map_err(|error| format!("invalid subresource URL: {error}"))?;
    let same_page_origin = current.origin().ascii_serialization() == request_origin;
    let credentials_allowed = match credentials_mode {
        crate::dynamic_script::ModuleCredentialsMode::Omit => false,
        crate::dynamic_script::ModuleCredentialsMode::SameOrigin => same_page_origin,
        crate::dynamic_script::ModuleCredentialsMode::Include => true,
    };
    Ok(browser_request_headers(
        options,
        &current,
        request_origin,
        cookies,
        credentials_allowed,
        cors_mode,
        referrer_source,
        referrer_policy,
        true,
        false,
    ))
}

/// Background top-level document navigation transport. Redirects are followed
/// manually so every hop rematches the shared Cookie Store under top-level
/// SameSite rules and strips sensitive headers when origin changes.
#[cfg(feature = "dynamic-js")]
pub(crate) fn fetch_document_bytes_async(
    url: &str,
    options: FetchOptions,
    site_for_cookies_url: String,
    cookies: crate::cookie_store_web::CookieSnapshot,
    referrer_source: String,
    referrer_policy: ScriptReferrerPolicy,
    max_body_bytes: usize,
) -> BytesFetchTask {
    let (tx, rx) = mpsc::channel();
    let url = url.to_string();
    let cancellation = FetchCancellation::default();
    let worker_cancellation = cancellation.clone();
    thread::spawn(move || {
        if let Err(error) = fetch_document_bytes_with_redirects(
            &url,
            &options,
            &site_for_cookies_url,
            &cookies,
            &referrer_source,
            referrer_policy,
            max_body_bytes,
            &worker_cancellation,
            &tx,
        ) {
            let _ = tx.send(DocumentFetchEvent::Error(error.to_string()));
        }
    });
    BytesFetchTask {
        receiver: rx,
        cancellation,
    }
}

#[cfg(feature = "dynamic-js")]
fn fetch_document_bytes_with_redirects(
    url: &str,
    options: &FetchOptions,
    site_for_cookies_url: &str,
    cookies: &crate::cookie_store_web::CookieSnapshot,
    referrer_source: &str,
    mut referrer_policy: ScriptReferrerPolicy,
    max_body_bytes: usize,
    cancellation: &FetchCancellation,
    sender: &mpsc::Sender<DocumentFetchEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(options.method, Method::Get) || options.body.is_some() {
        return Err("document navigations must use GET without a request body".into());
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(
            options.timeout_ms.unwrap_or(30_000),
        )))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let initial = url::Url::parse(url)?;
    validate_http_url_without_credentials(&initial, "document URL")?;
    let mut current = initial;
    let mut redirected = false;
    let mut set_cookies = Vec::new();
    let mut redirect_cookies = cookies.clone();
    let mut authorization_allowed = true;

    for _ in 0..=10 {
        cancellation.check()?;
        let mut request = agent.get(current.as_str());
        for (name, value) in &options.headers {
            if name.eq_ignore_ascii_case("cookie")
                || name.eq_ignore_ascii_case("referer")
                || (!authorization_allowed && name.eq_ignore_ascii_case("authorization"))
            {
                continue;
            }
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(referrer) =
            script_referrer_header(referrer_source, current.as_str(), referrer_policy)
        {
            request = request.header("Referer", referrer);
        }
        let cookie = redirect_cookies.header_for_top_level_navigation(
            current.as_str(),
            site_for_cookies_url,
            true,
        );
        if !cookie.is_empty() {
            request = request.header("Cookie", cookie);
        }
        let response = request.call()?;
        cancellation.check()?;
        let status = response.status().as_u16();
        let headers = response_headers(&response);
        let response_cookies = response_set_cookies(&response);
        for cookie in response_cookies {
            redirect_cookies.apply_http_set_cookie(current.as_str(), &cookie);
            set_cookies.push((current.to_string(), cookie));
        }
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location_values = response.headers().get_all("location");
            let mut locations = location_values.iter();
            let location = locations
                .next()
                .ok_or("redirect response is missing a Location header")?
                .to_str()
                .map_err(|_| "redirect response has an invalid Location header")?
                .to_string();
            if locations.next().is_some() {
                return Err("redirect response has multiple Location headers".into());
            }
            if let Some(policy) = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("referrer-policy"))
                .and_then(|(_, value)| ScriptReferrerPolicy::from_header(value))
            {
                referrer_policy = policy;
            }
            let next = current.join(&location)?;
            validate_http_url_without_credentials(&next, "document redirect URL")?;
            if current.origin() != next.origin() {
                // Once Fetch crosses an origin boundary, author credentials
                // stay removed even if a later redirect returns to the
                // original origin.
                authorization_allowed = false;
            }
            current = next;
            redirected = true;
            continue;
        }

        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("Unknown")
            .to_string();
        sender
            .send(DocumentFetchEvent::Response(FetchBytesResponse {
                status,
                ok: (200..300).contains(&status),
                status_text,
                headers,
                url: current.to_string(),
                redirected,
                set_cookies,
            }))
            .map_err(|_| "document fetch receiver disconnected")?;
        stream_bytes_with_cancellation(
            response.into_body().into_reader(),
            cancellation,
            max_body_bytes,
            sender,
        )?;
        sender
            .send(DocumentFetchEvent::Complete)
            .map_err(|_| "document fetch receiver disconnected")?;
        return Ok(());
    }
    Err("too many document redirects".into())
}

fn fetch_text_with_cancellation(
    url: &str,
    options: &FetchOptions,
    cancellation: &FetchCancellation,
    completion_order: &AtomicU64,
) -> Result<FetchTextResponse, String> {
    use ureq::ResponseExt as _;

    cancellation.check()?;
    let response = send_request(url, options).map_err(|error| error.to_string())?;
    cancellation.check()?;
    mark_text_fetch_completion(completion_order);
    let status = response.status().as_u16();
    let status_text = response
        .status()
        .canonical_reason()
        .unwrap_or("Unknown")
        .to_string();
    let final_url = response.get_uri().to_string();
    let redirected = response
        .get_redirect_history()
        .is_some_and(|history| history.len() > 1);
    let headers = response_headers(&response);
    let set_cookies = response_set_cookies(&response)
        .into_iter()
        .map(|cookie| (final_url.clone(), cookie))
        .collect();
    let body = read_text_with_cancellation(response.into_body().into_reader(), cancellation)?;
    Ok(FetchTextResponse {
        status,
        ok: (200..300).contains(&status),
        status_text,
        headers,
        url: final_url,
        redirected,
        set_cookies,
        body,
    })
}

fn fetch_browser_bytes_with_redirects(
    url: &str,
    options: &FetchOptions,
    request_origin: &str,
    cookies: &crate::cookie_store_web::CookieSnapshot,
    credentials_mode: BrowserCredentialsMode,
    cors_mode: bool,
    preflight_enabled: bool,
    same_origin_only: bool,
    redirect_mode: BrowserRedirectMode,
    referrer_source: &str,
    mut referrer_policy: ScriptReferrerPolicy,
    cancellation: &FetchCancellation,
    completion_order: &AtomicU64,
) -> Result<BrowserFetchResponse, Box<dyn std::error::Error>> {
    let config = ureq::Agent::config_builder()
        // Match browser fetch: no default timeout. AbortSignal and an
        // explicitly supplied RequestInit.timeout still cancel the request.
        .timeout_global(options.timeout_ms.map(std::time::Duration::from_millis))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut current = url::Url::parse(url)?;
    validate_http_url_without_credentials(&current, "script URL")?;
    let mut redirected = false;
    let mut set_cookies = Vec::new();
    let mut redirect_cookies = cookies.clone();
    let mut authorization_allowed = current.origin().ascii_serialization() == request_origin;
    let mut method = options.method;
    let mut body = options.body.clone();
    let mut body_headers_removed = false;

    for _ in 0..=10 {
        cancellation.check()?;
        let current_origin = current.origin().ascii_serialization();
        let same_page_origin = current_origin == request_origin;
        if same_origin_only && !same_page_origin {
            return Err(format!(
                "same-origin Fetch blocked request to {} from {request_origin}",
                current.as_str()
            )
            .into());
        }
        let credentials_allowed = match credentials_mode {
            BrowserCredentialsMode::Omit => false,
            BrowserCredentialsMode::SameOrigin => same_page_origin,
            BrowserCredentialsMode::Include => true,
        };
        let headers = browser_request_headers(
            options,
            &current,
            request_origin,
            &redirect_cookies,
            credentials_allowed,
            cors_mode,
            referrer_source,
            referrer_policy,
            authorization_allowed,
            body_headers_removed,
        );
        if preflight_enabled && !same_page_origin {
            perform_cors_preflight_if_needed(
                &agent,
                &current,
                request_origin,
                credentials_mode,
                method,
                &headers,
                cancellation,
            )?;
        }
        let response =
            send_request_with_agent(&agent, current.as_str(), method, &headers, body.as_deref())?;
        cancellation.check()?;
        let status = response.status().as_u16();
        let headers = response_headers(&response);
        if is_redirect_status(status) {
            if cors_mode {
                validate_browser_cors_headers(
                    request_origin,
                    current.as_str(),
                    &headers,
                    credentials_mode,
                )
                .map_err(|error| {
                    format!(
                        "script CORS redirect check failed for {}: {error}",
                        current.as_str()
                    )
                })?;
            }
            if redirect_mode == BrowserRedirectMode::Error {
                return Err(format!(
                    "Fetch redirect mode is error for response from {}",
                    current.as_str()
                )
                .into());
            }
            if redirect_mode == BrowserRedirectMode::Manual {
                if credentials_allowed {
                    set_cookies.extend(
                        response_set_cookies(&response)
                            .into_iter()
                            .map(|cookie| (current.to_string(), cookie)),
                    );
                }
                let status_text = response
                    .status()
                    .canonical_reason()
                    .unwrap_or("Unknown")
                    .to_string();
                return Ok(BrowserFetchResponse {
                    status,
                    ok: false,
                    status_text,
                    headers,
                    url: current.to_string(),
                    redirected: false,
                    set_cookies,
                    body: Vec::new(),
                });
            }
            let location_values = response.headers().get_all("location");
            let mut locations = location_values.iter();
            let location = locations
                .next()
                .ok_or("redirect response is missing a Location header")?
                .to_str()
                .map_err(|_| "redirect response has an invalid Location header")?
                .to_string();
            if locations.next().is_some() {
                return Err("redirect response has multiple Location headers".into());
            }
            if let Some(policy) = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("referrer-policy"))
                .and_then(|(_, value)| ScriptReferrerPolicy::from_header(value))
            {
                referrer_policy = policy;
            }
            if credentials_allowed {
                for value in response.headers().get_all("set-cookie") {
                    if let Ok(value) = value.to_str() {
                        redirect_cookies.apply_http_set_cookie(current.as_str(), value);
                        set_cookies.push((current.to_string(), value.to_string()));
                    }
                }
            }
            let next = current.join(&location)?;
            validate_http_url_without_credentials(&next, "script redirect URL")?;
            if current.origin() != next.origin() {
                authorization_allowed = false;
            }
            if (status == 303 && !matches!(method, Method::Get | Method::Head))
                || (matches!(status, 301 | 302) && method == Method::Post)
            {
                method = Method::Get;
                body = None;
                body_headers_removed = true;
            }
            current = next;
            redirected = true;
            cancellation.check()?;
            continue;
        }

        mark_text_fetch_completion(completion_order);
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("Unknown")
            .to_string();
        if credentials_allowed {
            set_cookies.extend(
                response_set_cookies(&response)
                    .into_iter()
                    .map(|cookie| (current.to_string(), cookie)),
            );
        }
        let body = read_bytes_with_cancellation(response.into_body().into_reader(), cancellation)?;
        return Ok(BrowserFetchResponse {
            status,
            ok: (200..300).contains(&status),
            status_text,
            headers,
            url: current.to_string(),
            redirected,
            set_cookies,
            body,
        });
    }
    Err("too many browser request redirects".into())
}

#[allow(clippy::too_many_arguments)]
fn browser_request_headers(
    options: &FetchOptions,
    current: &url::Url,
    request_origin: &str,
    cookies: &crate::cookie_store_web::CookieSnapshot,
    credentials_allowed: bool,
    cors_mode: bool,
    referrer_source: &str,
    referrer_policy: ScriptReferrerPolicy,
    authorization_allowed: bool,
    body_headers_removed: bool,
) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for (name, value) in &options.headers {
        if name.eq_ignore_ascii_case("origin")
            || name.eq_ignore_ascii_case("cookie")
            || name.eq_ignore_ascii_case("referer")
            || (!authorization_allowed && name.eq_ignore_ascii_case("authorization"))
            || (body_headers_removed
                && matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-encoding"
                        | "content-language"
                        | "content-length"
                        | "content-location"
                        | "content-type"
                ))
        {
            continue;
        }
        headers.insert(name.clone(), value.clone());
    }
    if cors_mode {
        headers.insert("Origin".into(), request_origin.into());
    }
    if let Some(referrer) =
        script_referrer_header(referrer_source, current.as_str(), referrer_policy)
    {
        headers.insert("Referer".into(), referrer);
    }
    if credentials_allowed {
        let cookie = cookies.header_for_subresource(current.as_str(), request_origin);
        if !cookie.is_empty() {
            headers.insert("Cookie".into(), cookie);
        }
    }
    headers
}

fn fetch_browser_text_with_redirects(
    url: &str,
    options: &FetchOptions,
    request_origin: &str,
    cookies: &crate::cookie_store_web::CookieSnapshot,
    credentials_mode: BrowserCredentialsMode,
    cors_mode: bool,
    preflight_enabled: bool,
    same_origin_only: bool,
    redirect_mode: BrowserRedirectMode,
    referrer_source: &str,
    referrer_policy: ScriptReferrerPolicy,
    cancellation: &FetchCancellation,
    completion_order: &AtomicU64,
) -> Result<FetchTextResponse, Box<dyn std::error::Error>> {
    fetch_browser_bytes_with_redirects(
        url,
        options,
        request_origin,
        cookies,
        credentials_mode,
        cors_mode,
        preflight_enabled,
        same_origin_only,
        redirect_mode,
        referrer_source,
        referrer_policy,
        cancellation,
        completion_order,
    )?
    .into_text()
    .map_err(Into::into)
}

pub(crate) fn validate_browser_cors_headers(
    request_origin: &str,
    response_url: &str,
    headers: &HashMap<String, String>,
    credentials_mode: BrowserCredentialsMode,
) -> Result<(), String> {
    let response_origin = url::Url::parse(response_url)
        .map_err(|error| format!("invalid response URL {response_url}: {error}"))?
        .origin()
        .ascii_serialization();
    if response_origin == request_origin {
        return Ok(());
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };
    let allowed_origin = header("access-control-allow-origin");
    if credentials_mode == BrowserCredentialsMode::Include {
        let allows_credentials =
            header("access-control-allow-credentials").is_some_and(|value| value == "true");
        if allowed_origin == Some(request_origin) && allows_credentials {
            return Ok(());
        }
        return Err(format!(
            "expected an exact Access-Control-Allow-Origin {request_origin:?} and Access-Control-Allow-Credentials: true"
        ));
    }
    if matches!(allowed_origin, Some("*")) || allowed_origin == Some(request_origin) {
        return Ok(());
    }
    Err(format!("origin {request_origin} is not allowed"))
}

fn validate_http_url_without_credentials(
    url: &url::Url,
    context: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{context} must use http or https").into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{context} must not include credentials").into());
    }
    Ok(())
}

fn script_referrer_header(
    source: &str,
    target: &str,
    policy: ScriptReferrerPolicy,
) -> Option<String> {
    let mut source = url::Url::parse(source).ok()?;
    let target = url::Url::parse(target).ok()?;
    if !matches!(source.scheme(), "http" | "https") || !matches!(target.scheme(), "http" | "https")
    {
        return None;
    }
    source.set_fragment(None);
    let _ = source.set_username("");
    let _ = source.set_password(None);
    let same_origin = source.origin() == target.origin();
    let downgrade = source.scheme() == "https" && target.scheme() == "http";
    let full = source.to_string();
    let origin = format!(
        "{}/",
        source.origin().ascii_serialization().trim_end_matches('/')
    );

    match policy {
        ScriptReferrerPolicy::NoReferrer => None,
        ScriptReferrerPolicy::NoReferrerWhenDowngrade => (!downgrade).then_some(full),
        ScriptReferrerPolicy::Origin => Some(origin),
        ScriptReferrerPolicy::OriginWhenCrossOrigin => {
            Some(if same_origin { full } else { origin })
        }
        ScriptReferrerPolicy::SameOrigin => same_origin.then_some(full),
        ScriptReferrerPolicy::StrictOrigin => (!downgrade).then_some(origin),
        ScriptReferrerPolicy::StrictOriginWhenCrossOrigin => {
            if same_origin {
                Some(full)
            } else if downgrade {
                None
            } else {
                Some(origin)
            }
        }
        ScriptReferrerPolicy::UnsafeUrl => Some(full),
    }
}

fn mark_text_fetch_completion(completion_order: &AtomicU64) {
    let order = NEXT_TEXT_FETCH_COMPLETION.fetch_add(1, Ordering::Relaxed);
    let _ = completion_order.compare_exchange(0, order, Ordering::Release, Ordering::Relaxed);
}

fn response_headers(response: &ureq::http::Response<ureq::Body>) -> HashMap<String, String> {
    let mut headers = HashMap::<String, String>::new();
    for (name, value) in response.headers() {
        let Ok(value) = value.to_str() else {
            continue;
        };
        headers
            .entry(name.to_string())
            .and_modify(|combined| {
                combined.push_str(", ");
                combined.push_str(value);
            })
            .or_insert_with(|| value.to_string());
    }
    headers
}

fn response_set_cookies(response: &ureq::http::Response<ureq::Body>) -> Vec<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToOwned::to_owned))
        .collect()
}

fn read_text_with_cancellation(
    reader: impl std::io::Read,
    cancellation: &FetchCancellation,
) -> Result<String, String> {
    String::from_utf8(read_bytes_with_cancellation(reader, cancellation)?)
        .map_err(|error| error.to_string())
}

fn read_bytes_with_cancellation(
    mut reader: impl std::io::Read,
    cancellation: &FetchCancellation,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        cancellation.check()?;
        let read = reader.read(&mut chunk).map_err(|error| error.to_string())?;
        cancellation.check()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

#[cfg(feature = "dynamic-js")]
fn stream_bytes_with_cancellation(
    mut reader: impl std::io::Read,
    cancellation: &FetchCancellation,
    max_body_bytes: usize,
    sender: &mpsc::Sender<DocumentFetchEvent>,
) -> Result<(), String> {
    let mut received = 0_usize;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        cancellation.check()?;
        let read = reader.read(&mut chunk).map_err(|error| error.to_string())?;
        cancellation.check()?;
        if read == 0 {
            break;
        }
        if received.saturating_add(read) > max_body_bytes {
            return Err(format!(
                "document response exceeds body limit (>{max_body_bytes} bytes)"
            ));
        }
        received += read;
        sender
            .send(DocumentFetchEvent::BodyChunk(chunk[..read].to_vec()))
            .map_err(|_| "document fetch receiver disconnected".to_string())?;
    }
    Ok(())
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn enable_page_http_cache(directory: &std::path::Path) {
        set_page_http_cache_policy(crate::browser_http_cache::CachePolicy {
            directory: Some(directory.to_path_buf()),
            max_entries: 16,
            max_bytes: 1024 * 1024,
            max_body_bytes: 1024,
        });
    }

    fn http_fixture(
        response_body: &'static str,
        expected_request: &'static str,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP fixture");
        let address = listener.local_addr().expect("read fixture address");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture request");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("set fixture read timeout");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 2048];
            while !String::from_utf8_lossy(&request).contains(expected_request) {
                let read = stream.read(&mut chunk).expect("read fixture request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let request = String::from_utf8_lossy(&request);
            assert!(
                request.contains(expected_request),
                "fixture request did not contain {expected_request:?}: {request}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture response");
        });
        (format!("http://{address}/fixture"), handle)
    }

    #[test]
    fn fetch_preserves_http_error_status_and_response_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP error fixture");
        let address = listener.local_addr().expect("HTTP error fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept HTTP error request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 2\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlater",
                )
                .expect("write HTTP error response");
        });

        let response = fetch(
            &format!("http://{address}/unavailable"),
            FetchOptions::default(),
        );
        server.join().expect("HTTP error fixture completed");
        assert_eq!(response.status, 503);
        assert!(!response.ok);
        assert_eq!(response.header("retry-after"), Some("2"));
        assert_eq!(response.text().unwrap(), "later");
    }

    #[test]
    fn page_fetch_uses_and_updates_the_shared_cookie_store() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind page fetch fixture");
        let address = listener.local_addr().expect("page fetch fixture address");
        let page_url = format!("http://{address}/page/index.html");
        let data_url = format!("http://{address}/data");
        crate::cookie_store_web::set_active_url(&page_url);
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &data_url,
            "client=ready; Path=/",
            true,
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept page fetch request");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).expect("read page fetch request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("cookie: client=ready"),
                "{request}"
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nSet-Cookie: server=stored; Path=/\r\nContent-Length: 6\r\nConnection: close\r\n\r\nshared",
                )
                .expect("write page fetch response");
        });

        let response = fetch_value(vec![Value::from(data_url.clone())]);
        server.join().expect("page fetch fixture completed");
        assert_eq!(response.get_property("status").to_number(), 200.0);
        assert_eq!(
            response.call_method("text", vec![]).to_js_string(),
            "shared"
        );
        assert!(
            response
                .get_property("headers")
                .call_method("get", vec![Value::from("set-cookie")])
                .is_null()
        );
        assert!(
            crate::cookie_store_web::cookie_header_for_url(&data_url).contains("server=stored")
        );
    }

    #[test]
    fn page_fetch_enforces_cors_through_the_shared_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind page CORS fixture");
        let address = listener.local_addr().expect("page CORS fixture address");
        crate::cookie_store_web::set_active_url("http://page.example.test/index.html");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept page CORS request");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).expect("read page CORS request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("origin: http://page.example.test"),
                "{request}"
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nblocked",
                )
                .expect("write page CORS response");
        });

        let response = fetch_value(vec![Value::from(format!("http://{address}/data"))]);
        server.join().expect("page CORS fixture completed");
        assert_eq!(response.get_property("status").to_number(), 0.0);
        assert_eq!(response.get_property("type").to_js_string(), "error");
        assert!(
            response
                .get_property("statusText")
                .to_js_string()
                .contains("Fetch CORS check failed")
        );
    }

    #[test]
    fn page_fetch_filters_cross_origin_response_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind CORS filtering fixture");
        let address = listener
            .local_addr()
            .expect("CORS filtering fixture address");
        crate::cookie_store_web::set_active_url("http://page.example.test/index.html");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept CORS filtering request");
            let mut request = [0_u8; 2048];
            let _ = stream
                .read(&mut request)
                .expect("read CORS filtering request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Expose-Headers: X-Visible\r\nX-Visible: yes\r\nX-Hidden: no\r\nContent-Type: text/plain\r\nContent-Length: 7\r\nConnection: close\r\n\r\nallowed",
                )
                .expect("write CORS filtering response");
        });

        let response = fetch_value(vec![Value::from(format!("http://{address}/data"))]);
        server.join().expect("CORS filtering fixture completed");
        let headers = response.get_property("headers");
        assert_eq!(response.get_property("type").to_js_string(), "cors");
        assert_eq!(
            headers
                .call_method("get", vec![Value::from("x-visible")])
                .to_js_string(),
            "yes"
        );
        assert!(
            headers
                .call_method("get", vec![Value::from("x-hidden")])
                .is_null()
        );
        assert!(
            headers
                .call_method("get", vec![Value::from("access-control-allow-origin")])
                .is_null()
        );
        assert_eq!(
            response.call_method("text", vec![]).to_js_string(),
            "allowed"
        );
    }

    #[test]
    fn page_fetch_revalidates_binary_response_through_shared_cache() {
        let directory = tempfile::tempdir().expect("create page cache directory");
        enable_page_http_cache(directory.path());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind page cache fixture");
        let address = listener.local_addr().expect("page cache fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let expected_body = vec![0, 0xff, 0x41, 0x80];
        let response_body = expected_body.clone();
        let server = std::thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept page cache request");
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).expect("read page cache request");
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                if request_index == 0 {
                    assert!(!request.contains("if-none-match:"), "{request}");
                    let headers = b"HTTP/1.1 200 OK\r\nETag: \"binary-v1\"\r\nContent-Type: application/octet-stream\r\nContent-Length: 4\r\nConnection: close\r\n\r\n";
                    stream
                        .write_all(headers)
                        .expect("write cache response headers");
                    stream
                        .write_all(&response_body)
                        .expect("write binary cache response");
                } else {
                    assert!(
                        request.contains("if-none-match: \"binary-v1\""),
                        "{request}"
                    );
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"binary-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write page cache revalidation");
                }
            }
        });

        let url = format!("http://{address}/binary");
        for _ in 0..2 {
            let response = fetch_value(vec![Value::from(url.clone())]);
            assert_eq!(response.get_property("status").to_u32(), 200);
            assert_eq!(
                w3cos_core::binary::bytes_of(&response.call_method("arrayBuffer", vec![])),
                Some(expected_body.clone())
            );
        }
        server.join().expect("page cache fixture completed");
    }

    #[test]
    fn page_fetch_cache_is_partitioned_by_credentials_mode() {
        let directory = tempfile::tempdir().expect("create partitioned page cache directory");
        enable_page_http_cache(directory.path());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cache partition fixture");
        let address = listener
            .local_addr()
            .expect("cache partition fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept partitioned request");
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).expect("read partitioned request");
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                match request_index {
                    0 => {
                        assert!(!request.contains("if-none-match:"), "{request}");
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nETag: \"omit-v1\"\r\nContent-Length: 4\r\nConnection: close\r\n\r\nomit",
                            )
                            .expect("write omit cache response");
                    }
                    1 => {
                        assert!(!request.contains("if-none-match:"), "{request}");
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nETag: \"include-v1\"\r\nContent-Length: 7\r\nConnection: close\r\n\r\ninclude",
                            )
                            .expect("write include cache response");
                    }
                    _ => {
                        assert!(request.contains("if-none-match: \"omit-v1\""), "{request}");
                        stream
                            .write_all(
                                b"HTTP/1.1 304 Not Modified\r\nETag: \"omit-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("write omit cache revalidation");
                    }
                }
            }
        });

        let url = format!("http://{address}/partitioned");
        let request = |credentials: &str| {
            fetch_value(vec![
                Value::from(url.clone()),
                Value::object(HashMap::from([(
                    "credentials".into(),
                    Value::from(credentials),
                )])),
            ])
        };
        assert_eq!(
            request("omit").call_method("text", vec![]).to_js_string(),
            "omit"
        );
        assert_eq!(
            request("include")
                .call_method("text", vec![])
                .to_js_string(),
            "include"
        );
        assert_eq!(
            request("omit").call_method("text", vec![]).to_js_string(),
            "omit"
        );
        server.join().expect("cache partition fixture completed");
    }

    #[test]
    fn page_fetch_cache_honors_vary_request_headers() {
        let directory = tempfile::tempdir().expect("create varied page cache directory");
        enable_page_http_cache(directory.path());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind varied cache fixture");
        let address = listener.local_addr().expect("varied cache fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            for expected_theme in ["dark", "light"] {
                let (mut stream, _) = listener.accept().expect("accept varied cache request");
                let mut request = [0_u8; 4096];
                let read = stream
                    .read(&mut request)
                    .expect("read varied cache request");
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                assert!(
                    request.contains(&format!("x-theme: {expected_theme}")),
                    "{request}"
                );
                assert!(!request.contains("if-none-match:"), "{request}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nETag: \"{expected_theme}-v1\"\r\nVary: X-Theme\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{expected_theme}",
                    expected_theme.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write varied cache response");
            }
        });

        let url = format!("http://{address}/theme");
        for theme in ["dark", "light"] {
            let response = fetch_value(vec![
                Value::from(url.clone()),
                Value::object(HashMap::from([(
                    "headers".into(),
                    Value::object(HashMap::from([("X-Theme".into(), Value::from(theme))])),
                )])),
            ]);
            assert_eq!(response.call_method("text", vec![]).to_js_string(), theme);
        }
        server.join().expect("varied cache fixture completed");
    }

    #[test]
    fn page_fetch_redirect_rematches_shared_cookies() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind page redirect fixture");
        let address = listener
            .local_addr()
            .expect("page redirect fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("accept redirect source");
            let mut request = [0_u8; 2048];
            let _ = first.read(&mut request).expect("read redirect source");
            first
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final\r\nSet-Cookie: hop=ready; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect source");

            let (mut second, _) = listener.accept().expect("accept redirect target");
            let read = second.read(&mut request).expect("read redirect target");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request.to_ascii_lowercase().contains("cookie: hop=ready"),
                "{request}"
            );
            second
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfinal",
                )
                .expect("write redirect target");
        });

        let response = fetch_value(vec![Value::from(format!("http://{address}/start"))]);
        server.join().expect("page redirect fixture completed");
        assert_eq!(response.call_method("text", vec![]).to_js_string(), "final");
        assert!(response.get_property("redirected").to_bool());
        assert_eq!(
            response.get_property("url").to_js_string(),
            format!("http://{address}/final")
        );
    }

    #[test]
    fn page_fetch_post_redirect_rewrites_method_and_body_headers_once() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind POST redirect fixture");
        let address = listener
            .local_addr()
            .expect("POST redirect fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("accept POST redirect source");
            let mut request = [0_u8; 4096];
            let read = first.read(&mut request).expect("read POST redirect source");
            let source = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(source.starts_with("post /start "), "{source}");
            assert!(
                source.contains("content-type: application/custom"),
                "{source}"
            );
            first
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write POST redirect source");

            let (mut second, _) = listener.accept().expect("accept POST redirect target");
            let read = second
                .read(&mut request)
                .expect("read POST redirect target");
            let target = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(target.starts_with("get /final "), "{target}");
            assert!(!target.contains("content-type:"), "{target}");
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write POST redirect target");
        });

        let init = Value::object(HashMap::from([
            ("method".into(), Value::from("POST")),
            ("body".into(), Value::from("payload")),
            (
                "headers".into(),
                Value::object(HashMap::from([(
                    "Content-Type".into(),
                    Value::from("application/custom"),
                )])),
            ),
        ]));
        let response = fetch_value(vec![Value::from(format!("http://{address}/start")), init]);
        server.join().expect("POST redirect fixture completed");
        assert_eq!(response.call_method("text", vec![]).to_js_string(), "ok");
    }

    #[test]
    fn page_fetch_same_origin_mode_blocks_before_network_io() {
        crate::cookie_store_web::set_active_url("http://page.example.test/index.html");
        let init = Value::object(HashMap::from([("mode".into(), Value::from("same-origin"))]));
        let response = fetch_value(vec![Value::from("http://127.0.0.1:9/data"), init]);
        assert_eq!(response.get_property("status").to_number(), 0.0);
        assert!(
            response
                .get_property("statusText")
                .to_js_string()
                .contains("same-origin Fetch blocked")
        );
    }

    #[test]
    fn page_fetch_no_cors_returns_an_opaque_cross_origin_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind no-cors fixture");
        let address = listener.local_addr().expect("no-cors fixture address");
        crate::cookie_store_web::set_active_url("http://page.example.test/index.html");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept no-cors request");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).expect("read no-cors request");
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(!request.contains("\r\norigin:"), "{request}");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nX-Private: hidden\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
                )
                .expect("write no-cors response");
        });

        let init = Value::object(HashMap::from([("mode".into(), Value::from("no-cors"))]));
        let response = fetch_value(vec![Value::from(format!("http://{address}/data")), init]);
        server.join().expect("no-cors fixture completed");
        assert_eq!(response.get_property("type").to_js_string(), "opaque");
        assert_eq!(response.get_property("status").to_number(), 0.0);
        assert_eq!(response.call_method("text", vec![]).to_js_string(), "");
        assert!(
            response
                .get_property("headers")
                .call_method("get", vec![Value::from("x-private")])
                .is_null()
        );
    }

    #[test]
    fn page_fetch_redirect_modes_error_and_manual_do_not_follow() {
        for (mode, expected_type) in [("error", "error"), ("manual", "opaqueredirect")] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect-mode fixture");
            let address = listener
                .local_addr()
                .expect("redirect-mode fixture address");
            crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept redirect-mode request");
                let mut request = [0_u8; 2048];
                let _ = stream
                    .read(&mut request)
                    .expect("read redirect-mode request");
                stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: /must-not-load\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("write redirect-mode response");
                listener
                    .set_nonblocking(true)
                    .expect("set redirect fixture nonblocking");
                std::thread::sleep(std::time::Duration::from_millis(20));
                assert!(
                    listener.accept().is_err(),
                    "redirect mode unexpectedly followed Location"
                );
            });

            let init = Value::object(HashMap::from([("redirect".into(), Value::from(mode))]));
            let response = fetch_value(vec![Value::from(format!("http://{address}/start")), init]);
            server.join().expect("redirect-mode fixture completed");
            assert_eq!(response.get_property("type").to_js_string(), expected_type);
            assert_eq!(response.get_property("status").to_number(), 0.0);
        }
    }

    #[test]
    fn page_fetch_performs_and_validates_cors_preflight() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind preflight fixture");
        let address = listener.local_addr().expect("preflight fixture address");
        let page_origin = "http://page.example.test";
        crate::cookie_store_web::set_active_url(&format!("{page_origin}/index.html"));
        let server = std::thread::spawn(move || {
            let (mut preflight, _) = listener.accept().expect("accept preflight request");
            let mut request_bytes = [0_u8; 4096];
            let read = preflight
                .read(&mut request_bytes)
                .expect("read preflight request");
            let preflight_request =
                String::from_utf8_lossy(&request_bytes[..read]).to_ascii_lowercase();
            assert!(
                preflight_request.starts_with("options /data "),
                "{preflight_request}"
            );
            assert!(
                preflight_request.contains("access-control-request-method: patch"),
                "{preflight_request}"
            );
            assert!(
                preflight_request.contains("access-control-request-headers: x-token"),
                "{preflight_request}"
            );
            preflight
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: http://page.example.test\r\nAccess-Control-Allow-Methods: PATCH\r\nAccess-Control-Allow-Headers: X-Token\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write preflight response");

            let (mut actual, _) = listener.accept().expect("accept actual CORS request");
            let read = actual
                .read(&mut request_bytes)
                .expect("read actual CORS request");
            let actual_request =
                String::from_utf8_lossy(&request_bytes[..read]).to_ascii_lowercase();
            assert!(
                actual_request.starts_with("patch /data "),
                "{actual_request}"
            );
            assert!(
                actual_request.contains("x-token: ready"),
                "{actual_request}"
            );
            actual
                .write_all(
                    b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: http://page.example.test\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                )
                .expect("write actual CORS response");
        });

        let init = Value::object(HashMap::from([
            ("method".into(), Value::from("PATCH")),
            (
                "headers".into(),
                Value::object(HashMap::from([("X-Token".into(), Value::from("ready"))])),
            ),
        ]));
        let response = fetch_value(vec![Value::from(format!("http://{address}/data")), init]);
        server.join().expect("preflight fixture completed");
        assert_eq!(response.get_property("status").to_number(), 200.0);
        assert_eq!(response.get_property("type").to_js_string(), "cors");
        assert_eq!(response.call_method("text", vec![]).to_js_string(), "ok");
    }

    #[test]
    fn page_fetch_rejects_failed_preflight_before_the_actual_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind rejected preflight fixture");
        let address = listener
            .local_addr()
            .expect("rejected preflight fixture address");
        crate::cookie_store_web::set_active_url("http://page.example.test/index.html");
        let server = std::thread::spawn(move || {
            let (mut preflight, _) = listener.accept().expect("accept rejected preflight");
            let mut request = [0_u8; 2048];
            let _ = preflight
                .read(&mut request)
                .expect("read rejected preflight");
            preflight
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: http://page.example.test\r\nAccess-Control-Allow-Methods: PATCH\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write rejected preflight");
            listener
                .set_nonblocking(true)
                .expect("set rejected preflight fixture nonblocking");
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert!(
                listener.accept().is_err(),
                "failed preflight unexpectedly sent the actual request"
            );
        });

        let init = Value::object(HashMap::from([
            ("method".into(), Value::from("PATCH")),
            (
                "headers".into(),
                Value::object(HashMap::from([("X-Token".into(), Value::from("blocked"))])),
            ),
        ]));
        let response = fetch_value(vec![Value::from(format!("http://{address}/data")), init]);
        server.join().expect("rejected preflight fixture completed");
        assert_eq!(response.get_property("status").to_number(), 0.0);
        assert!(
            response
                .get_property("statusText")
                .to_js_string()
                .contains("did not allow request headers")
        );
    }

    #[test]
    fn page_fetch_reuses_a_successful_preflight_cache_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cached preflight fixture");
        let address = listener
            .local_addr()
            .expect("cached preflight fixture address");
        crate::cookie_store_web::set_active_url("http://cached-page.example.test/index.html");
        let server = std::thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept cached preflight request");
                let mut request = [0_u8; 2048];
                let read = stream
                    .read(&mut request)
                    .expect("read cached preflight request");
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                if request_index == 0 {
                    assert!(request.starts_with("options /data "), "{request}");
                    stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: http://cached-page.example.test\r\nAccess-Control-Allow-Methods: PATCH\r\nAccess-Control-Allow-Headers: X-Token\r\nAccess-Control-Max-Age: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write cached preflight response");
                } else {
                    assert!(request.starts_with("patch /data "), "{request}");
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: http://cached-page.example.test\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .expect("write cached actual response");
                }
            }
        });

        let init = Value::object(HashMap::from([
            ("method".into(), Value::from("PATCH")),
            (
                "headers".into(),
                Value::object(HashMap::from([("X-Token".into(), Value::from("ready"))])),
            ),
        ]));
        for _ in 0..2 {
            let response = fetch_value(vec![
                Value::from(format!("http://{address}/data")),
                init.clone(),
            ]);
            assert_eq!(response.get_property("status").to_number(), 200.0);
        }
        server.join().expect("cached preflight fixture completed");
    }

    #[test]
    fn cors_preflight_cache_is_bounded_and_zero_age_is_not_stored() {
        *cors_preflight_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = CorsPreflightCache::default();
        for index in 0..=MAX_CORS_PREFLIGHT_CACHE_ENTRIES {
            store_cors_preflight_cache_entry(
                CorsPreflightCacheKey {
                    request_origin: format!("https://page-{index}.example"),
                    target_origin: "https://api.example".into(),
                    credentials_mode: BrowserCredentialsMode::SameOrigin,
                },
                HashSet::from(["PATCH".into()]),
                HashSet::from(["x-token".into()]),
                false,
                false,
                60,
            );
        }
        assert_eq!(
            cors_preflight_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entries
                .len(),
            MAX_CORS_PREFLIGHT_CACHE_ENTRIES
        );
        store_cors_preflight_cache_entry(
            CorsPreflightCacheKey {
                request_origin: "https://zero-age.example".into(),
                target_origin: "https://api.example".into(),
                credentials_mode: BrowserCredentialsMode::SameOrigin,
            },
            HashSet::from(["PATCH".into()]),
            HashSet::new(),
            false,
            false,
            0,
        );
        assert!(
            !cors_preflight_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entries
                .keys()
                .any(|key| key.request_origin == "https://zero-age.example")
        );
    }

    #[test]
    fn page_fetch_no_cors_rejects_unsafe_method_and_headers_before_io() {
        crate::cookie_store_web::set_active_url("http://page.example.test/index.html");
        for init in [
            Value::object(HashMap::from([
                ("mode".into(), Value::from("no-cors")),
                ("method".into(), Value::from("PATCH")),
            ])),
            Value::object(HashMap::from([
                ("mode".into(), Value::from("no-cors")),
                (
                    "headers".into(),
                    Value::object(HashMap::from([("X-Token".into(), Value::from("blocked"))])),
                ),
            ])),
        ] {
            let response = fetch_value(vec![Value::from("http://127.0.0.1:9/data"), init]);
            assert_eq!(response.get_property("status").to_number(), 0.0);
            assert!(
                response
                    .get_property("statusText")
                    .to_js_string()
                    .contains("no-cors Fetch does not allow")
            );
        }
    }

    #[test]
    fn fetch_value_exposes_browser_response_properties() {
        let response = response_value(
            FetchResponse {
                status: 200,
                ok: true,
                status_text: "OK".into(),
                headers: HashMap::from([("content-type".into(), "application/json".into())]),
                url: "https://cdn.example.test/final".into(),
                redirected: true,
                body_stream: ReadableStream::from_bytes(br#"{"token":"ok"}"#.to_vec()),
            },
            "https://example.test/data".into(),
        );

        assert!(response.get_property("ok").to_bool());
        assert_eq!(response.get_property("status").to_number(), 200.0);
        assert!(response.get_property("redirected").to_bool());
        assert_eq!(
            response.get_property("url").to_js_string(),
            "https://cdn.example.test/final"
        );
        assert_eq!(
            response
                .call_method("json", vec![])
                .get_property("token")
                .to_js_string(),
            "ok"
        );
        assert_eq!(
            response
                .get_property("headers")
                .call_method("get", vec![Value::from("Content-Type")])
                .to_js_string(),
            "application/json"
        );
        assert!(response.get_property("bodyUsed").to_bool());
    }

    #[test]
    fn fetch_promise_facade_supports_browser_promise_chains() {
        let response = fetch_promise_facade(response_from_bytes(
            br#"{"token":"ok"}"#.to_vec(),
            200,
            "OK".into(),
            headers_value_from_list(Rc::new(RefCell::new(Vec::new()))),
            "https://example.test/fixture".into(),
            "basic".into(),
        ));

        assert!(response.get_property("ok").to_bool());
        assert!(response.get_property("then").is_function());
        assert!(response.get_property("catch").is_function());
        assert!(response.get_property("finally").is_function());

        let log = Rc::new(RefCell::new(Vec::new()));
        let then_log = Rc::clone(&log);
        let finally_log = Rc::clone(&log);
        response
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    let resolved = args.first().cloned().unwrap_or(Value::Undefined);
                    assert!(
                        resolved.get_property("then").is_undefined(),
                        "Promise callbacks must receive the underlying Response"
                    );
                    then_log.borrow_mut().push(
                        resolved
                            .call_method("json", vec![])
                            .get_property("token")
                            .to_js_string(),
                    );
                    Value::Undefined
                })],
            )
            .call_method(
                "catch",
                vec![Value::function(|_, _| {
                    panic!("fulfilled fetch must not enter catch")
                })],
            )
            .call_method(
                "finally",
                vec![Value::function(move |_, _| {
                    finally_log.borrow_mut().push("finally".into());
                    Value::Undefined
                })],
            );

        assert!(log.borrow().is_empty(), "Promise callbacks are microtasks");
        assert_eq!(w3cos_core::promise::drain_microtasks(), 3);
        assert_eq!(log.borrow().as_slice(), &["ok", "finally"]);
    }

    #[test]
    fn headers_request_response_and_abort_controller_shapes() {
        let headers = w3cos_core::class::construct(
            &headers_class(),
            vec![Value::object(HashMap::from([(
                "X-Trace".into(),
                Value::from("one"),
            )]))],
        );
        headers.call_method("append", vec![Value::from("x-trace"), Value::from("two")]);
        assert_eq!(
            headers
                .call_method("get", vec![Value::from("X-TRACE")])
                .to_js_string(),
            "one, two"
        );

        let request = w3cos_core::class::construct(
            &request_class(),
            vec![
                Value::from("https://example.test/items"),
                Value::object(HashMap::from([
                    ("method".into(), Value::from("post")),
                    ("headers".into(), headers.clone()),
                    ("body".into(), Value::from(r#"{"id":1}"#)),
                    ("mode".into(), Value::from("same-origin")),
                    ("redirect".into(), Value::from("manual")),
                ])),
            ],
        );
        assert_eq!(request.get_property("method").to_js_string(), "POST");
        assert_eq!(request.get_property("mode").to_js_string(), "same-origin");
        assert_eq!(request.get_property("redirect").to_js_string(), "manual");
        assert_eq!(
            request
                .get_property("headers")
                .call_method("get", vec![Value::from("x-trace")])
                .to_js_string(),
            "one, two"
        );

        let response = w3cos_core::class::construct(
            &response_class(),
            vec![
                Value::from("created"),
                Value::object(HashMap::from([("status".into(), Value::Number(201.0))])),
            ],
        );
        assert_eq!(response.get_property("status").to_number(), 201.0);
        assert_eq!(
            response.call_method("text", vec![]).to_js_string(),
            "created"
        );

        let controller = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let signal = controller.get_property("signal");
        let called = Rc::new(Cell::new(false));
        let observed = Rc::clone(&called);
        signal.set_property(
            "onabort",
            Value::function(move |_, _| {
                observed.set(true);
                Value::Undefined
            }),
        );
        controller.call_method("abort", vec![Value::from("stopped")]);
        assert!(signal.get_property("aborted").to_bool());
        assert_eq!(signal.get_property("reason").to_js_string(), "stopped");
        assert!(called.get());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                signal.call_method("throwIfAborted", vec![]);
            }))
            .is_err()
        );

        let second = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let second_signal = second.get_property("signal");
        let abort_signal = abort_signal_class();
        let aggregate = abort_signal.call_method(
            "any",
            vec![Value::array(vec![signal.clone(), second_signal])],
        );
        assert!(aggregate.get_property("aborted").to_bool());
        assert_eq!(aggregate.get_property("reason").to_js_string(), "stopped");

        let later = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let propagated = abort_signal.call_method(
            "any",
            vec![Value::array(vec![later.get_property("signal")])],
        );
        assert!(!propagated.get_property("aborted").to_bool());
        later.call_method("abort", vec![Value::from("later")]);
        assert!(propagated.get_property("aborted").to_bool());
        assert_eq!(propagated.get_property("reason").to_js_string(), "later");

        let timeout = abort_signal.call_method("timeout", vec![Value::Number(125.0)]);
        assert!(!timeout.get_property("aborted").to_bool());
        assert_eq!(
            timeout.get_property("__w3cos_timeout_ms").to_number(),
            125.0
        );
    }

    #[test]
    fn fetch_classes_abort_callbacks_and_methods_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_headers_class = headers_class();
        let old_request_class = request_class();
        let old_response_class = response_class();
        let old_controller_class = abort_controller_class();
        let old_signal_class = abort_signal_class();
        assert!(old_headers_class.strict_eq(&headers_class()));
        assert!(old_request_class.strict_eq(&request_class()));
        assert!(old_response_class.strict_eq(&response_class()));

        let headers = w3cos_core::class::construct(
            &old_headers_class,
            vec![Value::object(HashMap::from([(
                "x-realm".into(),
                Value::string("old"),
            )]))],
        );
        let request = w3cos_core::class::construct(
            &old_request_class,
            vec![Value::string("https://realm.invalid/data")],
        );
        let response =
            w3cos_core::class::construct(&old_response_class, vec![Value::string("old body")]);
        let controller = w3cos_core::class::construct(&old_controller_class, vec![]);
        let signal = controller.get_property("signal");
        let callback_marker = Rc::new(());
        let callback_marker_weak = Rc::downgrade(&callback_marker);
        signal.call_method(
            "addEventListener",
            vec![
                Value::string("abort"),
                Value::function(move |_, _| {
                    let _ = &callback_marker;
                    Value::Undefined
                }),
            ],
        );
        let handler_marker = Rc::new(());
        let handler_marker_weak = Rc::downgrade(&handler_marker);
        signal.set_property(
            "onabort",
            Value::function(move |_, _| {
                let _ = &handler_marker;
                Value::Undefined
            }),
        );

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_headers_class.strict_eq(&headers_class()));
        assert!(!old_request_class.strict_eq(&request_class()));
        assert!(!old_response_class.strict_eq(&response_class()));
        assert!(!old_controller_class.strict_eq(&abort_controller_class()));
        assert!(!old_signal_class.strict_eq(&abort_signal_class()));
        for class in [
            old_headers_class,
            old_request_class,
            old_response_class,
            old_controller_class,
            old_signal_class,
        ] {
            assert!(class.call(Value::Undefined, vec![]).is_undefined());
        }
        assert!(
            headers
                .call_method("get", vec![Value::string("x-realm")])
                .is_undefined()
        );
        assert!(request.call_method("clone", vec![]).is_undefined());
        assert!(response.call_method("text", vec![]).is_undefined());
        assert!(controller.call_method("abort", vec![]).is_undefined());
        assert!(
            signal
                .call_method(
                    "addEventListener",
                    vec![
                        Value::string("abort"),
                        Value::function(|_, _| Value::Undefined)
                    ],
                )
                .is_undefined()
        );
        assert!(signal.get_property("onabort").is_null());
        assert!(callback_marker_weak.upgrade().is_none());
        assert!(handler_marker_weak.upgrade().is_none());

        let fresh = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        assert!(fresh.get_property("abort").is_function());
        reset_realm();
    }

    #[test]
    fn request_signal_is_inherited_and_pre_aborted_fetch_skips_io() {
        let controller = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let signal = controller.get_property("signal");
        let request = w3cos_core::class::construct(
            &request_class(),
            vec![
                Value::from("https://network-must-not-run.invalid/"),
                Value::object(HashMap::from([("signal".into(), signal.clone())])),
            ],
        );
        controller.call_method("abort", vec![Value::from("cancelled")]);

        let inherited =
            w3cos_core::class::construct(&request_class(), vec![request.clone(), Value::Undefined]);
        assert!(
            inherited
                .get_property("signal")
                .get_property("aborted")
                .to_bool()
        );

        let response = fetch_value(vec![inherited]);
        assert_eq!(response.get_property("status").to_number(), 0.0);
        assert_eq!(response.get_property("type").to_js_string(), "error");
        assert_eq!(
            response.get_property("statusText").to_js_string(),
            "AbortError: cancelled"
        );
    }

    #[test]
    fn page_fetch_abort_interrupts_waiting_for_response_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind header-abort fixture");
        let address = listener.local_addr().expect("header-abort fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept header-abort request");
            let mut request = [0_u8; 2048];
            let _ = stream
                .read(&mut request)
                .expect("read header-abort request");
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nlate",
            );
        });

        let controller = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let signal = controller.get_property("signal");
        let controller_for_timer = controller.clone();
        crate::jsdom::schedule_timeout_value(
            Value::function(move |_, _| {
                controller_for_timer.call_method("abort", vec![Value::from("cancelled-in-flight")]);
                Value::Undefined
            }),
            10,
        );
        let started = std::time::Instant::now();
        let response = fetch_value(vec![
            Value::from(format!("http://{address}/slow-headers")),
            Value::object(HashMap::from([
                ("signal".into(), signal.clone()),
                ("cache".into(), Value::from("no-store")),
            ])),
        ]);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "{elapsed:?}"
        );
        assert!(signal.get_property("aborted").to_bool());
        assert_eq!(response.get_property("type").to_js_string(), "error");
        assert_eq!(
            response.get_property("statusText").to_js_string(),
            "AbortError: cancelled-in-flight"
        );
        server.join().expect("header-abort fixture completed");
    }

    #[test]
    fn page_fetch_abort_interrupts_response_body_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind body-abort fixture");
        let address = listener.local_addr().expect("body-abort fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept body-abort request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read body-abort request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\npart")
                .expect("write first response-body chunk");
            stream.flush().expect("flush first response-body chunk");
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = stream.write_all(b"late");
        });

        let controller = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let signal = controller.get_property("signal");
        let controller_for_timer = controller.clone();
        crate::jsdom::schedule_timeout_value(
            Value::function(move |_, _| {
                controller_for_timer.call_method("abort", vec![Value::from("body-cancelled")]);
                Value::Undefined
            }),
            20,
        );
        let started = std::time::Instant::now();
        let response = fetch_value(vec![
            Value::from(format!("http://{address}/slow-body")),
            Value::object(HashMap::from([
                ("signal".into(), signal.clone()),
                ("cache".into(), Value::from("no-store")),
            ])),
        ]);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(110),
            "{elapsed:?}"
        );
        assert!(signal.get_property("aborted").to_bool());
        assert_eq!(
            response.get_property("statusText").to_js_string(),
            "AbortError: body-cancelled"
        );
        server.join().expect("body-abort fixture completed");
    }

    #[test]
    fn abort_signal_timeout_cancels_page_fetch_in_flight() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind timeout-abort fixture");
        let address = listener
            .local_addr()
            .expect("timeout-abort fixture address");
        crate::cookie_store_web::set_active_url(&format!("http://{address}/index.html"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept timeout-abort request");
            let mut request = [0_u8; 2048];
            let _ = stream
                .read(&mut request)
                .expect("read timeout-abort request");
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nlate",
            );
        });

        let signal = abort_signal_class().call_method("timeout", vec![Value::Number(10.0)]);
        let started = std::time::Instant::now();
        let response = fetch_value(vec![
            Value::from(format!("http://{address}/timeout")),
            Value::object(HashMap::from([
                ("signal".into(), signal.clone()),
                ("cache".into(), Value::from("no-store")),
            ])),
        ]);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "{elapsed:?}"
        );
        assert!(signal.get_property("aborted").to_bool());
        assert_eq!(
            response.get_property("statusText").to_js_string(),
            "TimeoutError: TimeoutError"
        );
        server.join().expect("timeout-abort fixture completed");
    }

    #[test]
    fn aot_fetch_abort_from_promise_interrupts_in_flight_native_request() {
        crate::cookie_store_web::reset_document_context();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind aot-abort fixture");
        let address = listener.local_addr().expect("aot-abort fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept aot-abort request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read aot-abort request");
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nlate",
            );
        });

        let controller = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let signal = controller.get_property("signal");
        let controller_for_promise = controller.clone();
        w3cos_core::promise::resolve(vec![Value::Undefined]).call_method(
            "then",
            vec![Value::function(move |_, _| {
                controller_for_promise.call_method("abort", vec![Value::from("from-promise")]);
                Value::Undefined
            })],
        );
        let started = std::time::Instant::now();
        let response = fetch_value(vec![
            Value::from(format!("http://{address}/aot-slow")),
            Value::object(HashMap::from([
                ("signal".into(), signal.clone()),
                ("cache".into(), Value::from("no-store")),
            ])),
        ]);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "{elapsed:?}"
        );
        assert!(signal.get_property("aborted").to_bool());
        assert_eq!(response.get_property("type").to_js_string(), "error");
        assert_eq!(
            response.get_property("statusText").to_js_string(),
            "AbortError: from-promise"
        );
        server.join().expect("aot-abort fixture completed");
    }

    #[test]
    fn aot_fetch_abort_from_timeout_interrupts_in_flight_native_request() {
        crate::cookie_store_web::reset_document_context();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind aot-timer-abort fixture");
        let address = listener
            .local_addr()
            .expect("aot-timer-abort fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept aot-timer-abort request");
            let mut request = [0_u8; 2048];
            let _ = stream
                .read(&mut request)
                .expect("read aot-timer-abort request");
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nlate",
            );
        });

        let controller = w3cos_core::class::construct(&abort_controller_class(), vec![]);
        let signal = controller.get_property("signal");
        let controller_for_timer = controller.clone();
        crate::jsdom::schedule_timeout_value(
            Value::function(move |_, _| {
                controller_for_timer.call_method("abort", vec![Value::from("from-timer")]);
                Value::Undefined
            }),
            10,
        );
        let started = std::time::Instant::now();
        let response = fetch_value(vec![
            Value::from(format!("http://{address}/aot-timer")),
            Value::object(HashMap::from([
                ("signal".into(), signal.clone()),
                ("cache".into(), Value::from("no-store")),
            ])),
        ]);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "{elapsed:?}"
        );
        assert!(signal.get_property("aborted").to_bool());
        assert_eq!(
            response.get_property("statusText").to_js_string(),
            "AbortError: from-timer"
        );
        server.join().expect("aot-timer-abort fixture completed");
    }

    #[test]
    fn fetch_resolves_blob_object_urls_and_revoke_invalidates_them() {
        let bytes = w3cos_core::binary::typed_array_value(vec![
            Value::Number(0.0),
            Value::Number(0xff as f64),
            Value::Number(0x41 as f64),
        ]);
        let blob = w3cos_core::class::construct(
            &w3cos_core::web::blob_class(),
            vec![
                Value::array(vec![bytes]),
                Value::object(HashMap::from([(
                    "type".into(),
                    Value::from("application/octet-stream"),
                )])),
            ],
        );
        let url_class = w3cos_core::web::url_class();
        let url = url_class
            .call_method("createObjectURL", vec![blob])
            .to_js_string();
        let response = fetch_value(vec![Value::from(url.clone())]);
        assert_eq!(response.get_property("status").to_u32(), 200);
        assert_eq!(
            response
                .get_property("headers")
                .call_method("get", vec![Value::from("content-type")])
                .to_js_string(),
            "application/octet-stream"
        );
        assert_eq!(
            w3cos_core::binary::bytes_of(&response.call_method("arrayBuffer", vec![])),
            Some(vec![0, 0xff, 0x41])
        );

        url_class.call_method("revokeObjectURL", vec![Value::from(url.clone())]);
        let revoked = fetch_value(vec![Value::from(url)]);
        assert_eq!(revoked.get_property("status").to_u32(), 0);
        assert_eq!(revoked.get_property("type").to_js_string(), "error");
        assert!(
            revoked
                .get_property("statusText")
                .to_js_string()
                .starts_with("NetworkError:")
        );
    }

    #[test]
    fn fetch_get_local_fixture() {
        let (url, fixture) = http_fixture(r#"{"ok":true}"#, "GET /fixture");
        let resp = fetch(&url, FetchOptions::default());
        fixture.join().expect("GET fixture completed");
        assert!(resp.ok, "status: {} {}", resp.status, resp.status_text);
        assert_eq!(resp.status, 200);
        assert!(!resp.text().unwrap().is_empty());
    }

    #[test]
    fn fetch_post_json() {
        let (url, fixture) = http_fixture(r#"{"data":"w3cos"}"#, r#"{"hello":"w3cos"}"#);
        let resp = fetch(
            &url,
            FetchOptions {
                method: Method::Post,
                body: Some(r#"{"hello":"w3cos"}"#.to_string()),
                ..Default::default()
            },
        );
        fixture.join().expect("POST fixture completed");
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
        let (url, fixture) = http_fixture(r#"{"ok":true}"#, "GET /fixture");
        let rx = fetch_async(&url, FetchOptions::default());
        let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        fixture.join().expect("async fixture completed");
        match result {
            FetchResult::Success(resp) => {
                assert!(resp.ok);
                assert_eq!(resp.status, 200);
            }
            FetchResult::Error(e) => panic!("fetch failed: {e}"),
        }
    }

    #[test]
    fn fetch_text_async_buffers_body_on_worker() {
        let (url, fixture) = http_fixture("export const ready = true;", "GET /fixture");
        let rx = fetch_text_async(&url, FetchOptions::default());
        let response = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("text fetch completed")
            .expect("text fetch succeeded");
        fixture.join().expect("text fixture completed");
        assert!(response.ok);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "export const ready = true;");
    }

    #[test]
    fn cancellable_text_fetch_stops_buffering_a_streaming_body() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cancellable body fixture");
        let address = listener.local_addr().expect("cancellable body address");
        let (body_started_tx, body_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let fixture = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept cancellable request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n",
                )
                .expect("write cancellable headers");
            stream
                .write_all(&vec![b'a'; 1024])
                .expect("write first body chunk");
            stream.flush().expect("flush first body chunk");
            body_started_tx.send(()).expect("signal body start");
            release_rx.recv().expect("release remaining body");
            // Unblock the client's pending `read()` so it can observe cancel
            // and drop the socket. A full 1 MiB burst can sit in kernel
            // buffers before that happens, which is not proof the client
            // buffered the body.
            let _ = stream.write_all(&[b'b'; 1024]);
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_millis(50));
            for _ in 0..32 {
                if stream.write_all(&[b'b'; 1024]).is_err() {
                    return true;
                }
                let _ = stream.flush();
            }
            false
        });

        let task = fetch_text_async_cancellable(
            &format!("http://{address}/stream.js"),
            FetchOptions::default(),
        );
        body_started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("body buffering started");
        task.cancel();
        release_tx.send(()).expect("release body fixture");
        let error = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("cancelled fetch worker completed")
            .expect_err("cancelled fetch must not publish a response");
        assert_eq!(error, SCRIPT_FETCH_CANCELLED);
        assert!(
            fixture.join().expect("cancellable fixture completed"),
            "client cancellation should close the body transport before one MiB is written"
        );
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn cancelling_module_fetch_between_redirect_hops_skips_the_target_request() {
        let source = TcpListener::bind("127.0.0.1:0").expect("bind redirect source");
        let source_address = source.local_addr().expect("redirect source address");
        let target = TcpListener::bind("127.0.0.1:0").expect("bind redirect target");
        let target_address = target.local_addr().expect("redirect target address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let fixture = std::thread::spawn(move || {
            let (mut stream, _) = source.accept().expect("accept redirect source request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            accepted_tx.send(()).expect("signal redirect request");
            release_rx.recv().expect("release redirect response");
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/target.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write redirect response");
        });

        let task = fetch_script_text_async(
            &format!("http://{source_address}/start.js"),
            FetchOptions::default(),
            format!("http://{source_address}"),
            crate::cookie_store_web::snapshot(),
            crate::dynamic_script::ModuleCredentialsMode::SameOrigin,
            true,
            format!("http://{source_address}/document.html"),
            ScriptReferrerPolicy::default(),
        );
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("redirect source requested");
        task.cancel();
        release_tx.send(()).expect("release redirect source");
        let error = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("redirect cancellation completed")
            .expect_err("cancelled redirect must not succeed");
        fixture.join().expect("redirect source completed");
        assert_eq!(error, SCRIPT_FETCH_CANCELLED);
        target
            .set_nonblocking(true)
            .expect("set target listener nonblocking");
        assert!(
            matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "cancelled module fetch must not open the next redirect hop"
        );
    }

    #[test]
    fn fetch_tracks_the_final_redirect_url() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect fixture");
        let address = listener.local_addr().expect("redirect fixture address");
        let fixture = std::thread::spawn(move || {
            let (mut redirect, _) = listener.accept().expect("accept redirect request");
            let mut request = [0_u8; 1024];
            let _ = redirect.read(&mut request);
            redirect
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect");

            let (mut final_response, _) = listener.accept().expect("accept final request");
            let mut request = [0_u8; 1024];
            let _ = final_response.read(&mut request);
            let body = "export const redirected = true;";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            final_response
                .write_all(response.as_bytes())
                .expect("write final response");
        });

        let response = fetch(
            &format!("http://{address}/start.js"),
            FetchOptions::default(),
        );
        fixture.join().expect("redirect fixture completed");
        assert!(response.ok);
        assert!(response.redirected);
        assert_eq!(response.url, format!("http://{address}/final.js"));
        assert_eq!(response.text().unwrap(), "export const redirected = true;");
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn script_referrer_policy_computes_browser_header_shapes() {
        let source = "https://user:secret@example.test/private/page.html?token=1#section";
        assert_eq!(
            script_referrer_header(
                source,
                "https://example.test/app.js",
                ScriptReferrerPolicy::default(),
            )
            .as_deref(),
            Some("https://example.test/private/page.html?token=1")
        );
        assert_eq!(
            script_referrer_header(
                source,
                "https://cdn.example.test/app.js",
                ScriptReferrerPolicy::default(),
            )
            .as_deref(),
            Some("https://example.test/")
        );
        assert_eq!(
            script_referrer_header(
                source,
                "http://cdn.example.test/app.js",
                ScriptReferrerPolicy::default(),
            ),
            None
        );
        assert_eq!(
            script_referrer_header(
                source,
                "https://cdn.example.test/app.js",
                ScriptReferrerPolicy::NoReferrer,
            ),
            None
        );
        assert_eq!(
            script_referrer_header(
                source,
                "http://cdn.example.test/app.js",
                ScriptReferrerPolicy::UnsafeUrl,
            )
            .as_deref(),
            Some("https://example.test/private/page.html?token=1")
        );
        assert_eq!(
            ScriptReferrerPolicy::from_header("invalid, origin"),
            Some(ScriptReferrerPolicy::Origin)
        );
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn redirect_response_referrer_policy_controls_the_next_hop() {
        let redirect = TcpListener::bind("127.0.0.1:0").expect("bind redirect source");
        let redirect_address = redirect.local_addr().expect("redirect source address");
        let target = TcpListener::bind("127.0.0.1:0").expect("bind redirect target");
        let target_address = target.local_addr().expect("redirect target address");
        let fixture = std::thread::spawn(move || {
            let (mut source_stream, _) = redirect.accept().expect("accept redirect source");
            let mut source_request = [0_u8; 2048];
            let source_read = source_stream
                .read(&mut source_request)
                .expect("read redirect source");
            let source_request = String::from_utf8_lossy(&source_request[..source_read]);
            assert!(
                source_request.to_ascii_lowercase().contains(&format!(
                    "referer: http://{redirect_address}/private/page.html?token=1"
                )),
                "initial request did not carry the full unsafe-url referrer: {source_request}"
            );
            write!(
                source_stream,
                "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/target.js\r\nReferrer-Policy: no-referrer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write redirect response");

            let (mut target_stream, _) = target.accept().expect("accept redirect target");
            let mut target_request = [0_u8; 2048];
            let target_read = target_stream
                .read(&mut target_request)
                .expect("read redirect target");
            let target_request = String::from_utf8_lossy(&target_request[..target_read]);
            assert!(
                !target_request.to_ascii_lowercase().contains("referer:"),
                "redirect response no-referrer policy was not applied: {target_request}"
            );
            target_stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect target");
        });

        let task = fetch_script_text_async(
            &format!("http://{redirect_address}/start.js"),
            FetchOptions::default(),
            format!("http://{redirect_address}"),
            crate::cookie_store_web::snapshot(),
            crate::dynamic_script::ModuleCredentialsMode::Include,
            false,
            format!("http://{redirect_address}/private/page.html?token=1#section"),
            ScriptReferrerPolicy::UnsafeUrl,
        );
        let response = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("redirect request completed")
            .expect("redirect request succeeded");
        fixture.join().expect("redirect fixture completed");
        assert!(response.ok);
        assert!(response.redirected);
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn redirect_round_trip_does_not_restore_authorization() {
        let original = TcpListener::bind("127.0.0.1:0").expect("bind original origin");
        let original_address = original.local_addr().expect("original origin address");
        let cross_origin = TcpListener::bind("127.0.0.1:0").expect("bind redirect origin");
        let cross_origin_address = cross_origin.local_addr().expect("redirect origin address");
        let fixture = std::thread::spawn(move || {
            let (mut first, _) = original.accept().expect("accept initial request");
            let mut request = [0_u8; 2048];
            let read = first.read(&mut request).expect("read initial request");
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(
                request.contains("authorization: bearer top-secret"),
                "initial same-origin request lost authorization: {request}"
            );
            write!(
                first,
                "HTTP/1.1 302 Found\r\nLocation: http://{cross_origin_address}/middle.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write first redirect");

            let (mut middle, _) = cross_origin.accept().expect("accept cross-origin request");
            let mut request = [0_u8; 2048];
            let read = middle
                .read(&mut request)
                .expect("read cross-origin request");
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(
                !request.contains("authorization:"),
                "cross-origin redirect leaked authorization: {request}"
            );
            write!(
                middle,
                "HTTP/1.1 302 Found\r\nLocation: http://{original_address}/final.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write return redirect");

            let (mut final_request, _) = original.accept().expect("accept return request");
            let mut request = [0_u8; 2048];
            let read = final_request
                .read(&mut request)
                .expect("read return request");
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(
                !request.contains("authorization:"),
                "origin round trip restored stripped authorization: {request}"
            );
            final_request
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write final response");
        });

        let task = fetch_script_text_async(
            &format!("http://{original_address}/start.js"),
            FetchOptions {
                headers: HashMap::from([("Authorization".into(), "Bearer top-secret".into())]),
                ..FetchOptions::default()
            },
            format!("http://{original_address}"),
            crate::cookie_store_web::snapshot(),
            crate::dynamic_script::ModuleCredentialsMode::Include,
            false,
            format!("http://{original_address}/document.html"),
            ScriptReferrerPolicy::default(),
        );
        let response = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("round-trip request completed")
            .expect("round-trip request succeeded");
        fixture.join().expect("round-trip fixture completed");
        assert!(response.ok);
        assert!(response.redirected);
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn no_cors_script_redirect_rejects_url_credentials_before_next_hop() {
        let source = TcpListener::bind("127.0.0.1:0").expect("bind redirect source");
        let source_address = source.local_addr().expect("redirect source address");
        let target = TcpListener::bind("127.0.0.1:0").expect("bind redirect target");
        let target_address = target.local_addr().expect("redirect target address");
        let fixture = std::thread::spawn(move || {
            let (mut stream, _) = source.accept().expect("accept redirect request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://user:password@{target_address}/target.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write credential-bearing redirect");
        });

        let task = fetch_script_text_async(
            &format!("http://{source_address}/start.js"),
            FetchOptions::default(),
            format!("http://{source_address}"),
            crate::cookie_store_web::snapshot(),
            crate::dynamic_script::ModuleCredentialsMode::Include,
            false,
            format!("http://{source_address}/document.html"),
            ScriptReferrerPolicy::default(),
        );
        let error = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("credential redirect completed")
            .expect_err("credential-bearing redirect must fail");
        fixture.join().expect("redirect fixture completed");
        assert!(error.contains("script redirect URL must not include credentials"));
        target
            .set_nonblocking(true)
            .expect("set target nonblocking");
        assert!(
            matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "credential-bearing redirect reached the target"
        );
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn script_redirect_rejects_multiple_location_headers() {
        let source = TcpListener::bind("127.0.0.1:0").expect("bind redirect source");
        let source_address = source.local_addr().expect("redirect source address");
        let first_target = TcpListener::bind("127.0.0.1:0").expect("bind first target");
        let first_target_address = first_target.local_addr().expect("first target address");
        let second_target = TcpListener::bind("127.0.0.1:0").expect("bind second target");
        let second_target_address = second_target.local_addr().expect("second target address");
        let fixture = std::thread::spawn(move || {
            let (mut stream, _) = source.accept().expect("accept redirect request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://{first_target_address}/first.js\r\nLocation: http://{second_target_address}/second.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write ambiguous redirect");
        });

        let task = fetch_script_text_async(
            &format!("http://{source_address}/start.js"),
            FetchOptions::default(),
            format!("http://{source_address}"),
            crate::cookie_store_web::snapshot(),
            crate::dynamic_script::ModuleCredentialsMode::Include,
            false,
            format!("http://{source_address}/document.html"),
            ScriptReferrerPolicy::default(),
        );
        let error = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("ambiguous redirect completed")
            .expect_err("ambiguous redirect must fail");
        fixture.join().expect("redirect fixture completed");
        assert!(error.contains("multiple Location headers"));
        for target in [&first_target, &second_target] {
            target
                .set_nonblocking(true)
                .expect("set target nonblocking");
            assert!(
                matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "ambiguous redirect reached a target"
            );
        }
    }

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn document_redirect_rejects_non_http_and_credential_urls() {
        let credential = url::Url::parse("https://user:password@example.test/document").unwrap();
        assert!(
            validate_http_url_without_credentials(&credential, "document redirect URL").is_err()
        );
        let local = url::Url::parse("file:///tmp/document.html").unwrap();
        assert!(validate_http_url_without_credentials(&local, "document redirect URL").is_err());
    }
}
