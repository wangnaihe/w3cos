use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
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
    let input = arguments.first().cloned().unwrap_or(Value::Undefined);
    let init = arguments.get(1).cloned().unwrap_or(Value::Undefined);
    let request_like = !input.get_property("url").is_undefined();
    let url = if request_like {
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
    let mut options = FetchOptions {
        method: parse_method(&method.to_js_string()),
        body: match body {
            Value::Undefined | Value::Null => None,
            body => Some(body.to_js_string()),
        },
        ..FetchOptions::default()
    };
    options.headers = headers_to_map(&headers);
    let timeout = init.get_property("timeout");
    if timeout.is_number() {
        options.timeout_ms = Some(timeout.to_number().max(0.0) as u64);
    }

    response_value(fetch(&url, options), url)
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
            vec![Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
        Value::function(move |this, args| {
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
            Value::function(move |_, _| {
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
    Value::function(|_, args| {
        let init = args.first().cloned().unwrap_or(Value::Undefined);
        headers_value_from_list(collect_header_init(&init))
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
    let body_used = Rc::new(Cell::new(false));
    let ok = (200..300).contains(&status);
    let mut props = HashMap::from([
        ("status".into(), Value::Number(status as f64)),
        ("ok".into(), Value::Bool(ok)),
        ("statusText".into(), Value::from(status_text.clone())),
        ("headers".into(), headers.clone()),
        ("url".into(), Value::from(url.clone())),
        ("type".into(), Value::from(response_type.clone())),
        ("redirected".into(), Value::Bool(false)),
        ("body".into(), Value::Null),
    ]);
    let body_used_getter = Rc::clone(&body_used);
    props.insert(
        "__w3cos_getter_bodyUsed".into(),
        Value::function(move |_, _| Value::Bool(body_used_getter.get())),
    );
    let text_body = body.clone();
    let text_used = Rc::clone(&body_used);
    props.insert(
        "text".into(),
        Value::function(move |_, _| {
            text_used.set(true);
            Value::from(text_body.clone())
        }),
    );
    let json_body = body.clone();
    let json_used = Rc::clone(&body_used);
    props.insert(
        "json".into(),
        Value::function(move |_, _| {
            json_used.set(true);
            w3cos_core::json::parse(vec![Value::from(json_body.clone())])
        }),
    );
    let bytes = body.as_bytes().to_vec();
    let bytes_used = Rc::clone(&body_used);
    props.insert(
        "arrayBuffer".into(),
        Value::function(move |_, _| {
            bytes_used.set(true);
            Value::array(
                bytes
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            )
        }),
    );
    let clone_body = body;
    props.insert(
        "clone".into(),
        Value::function(move |_, _| {
            response_from_parts(
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

fn response_value(response: FetchResponse, url: String) -> Value {
    let status = response.status;
    let status_text = response.status_text.clone();
    let headers = headers_value_from_list(Rc::new(RefCell::new(
        response
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )));
    let body = response.text().unwrap_or_default();
    response_from_parts(body, status, status_text, headers, url, "basic".into())
}

pub fn response_class() -> Value {
    let constructor = Value::function(|_, args| {
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
        let headers = headers_value_from_list(collect_header_init(&init.get_property("headers")));
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
        Value::function(|_, _| {
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
        Value::function(|_, args| {
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
        Value::function(|_, args| {
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
    constructor
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
    let signal = if init.get_property("signal").is_undefined() && inherited {
        input.get_property("signal")
    } else {
        init.get_property("signal")
    };
    let mut props = HashMap::from([
        ("url".into(), Value::from(url.clone())),
        ("method".into(), Value::from(method.clone())),
        ("headers".into(), headers.clone()),
        ("signal".into(), signal.clone()),
        ("body".into(), Value::Null),
        ("bodyUsed".into(), Value::Bool(false)),
        ("cache".into(), Value::from("default")),
        ("credentials".into(), Value::from("same-origin")),
        ("destination".into(), Value::from("")),
        ("integrity".into(), Value::from("")),
        ("mode".into(), Value::from("cors")),
        ("redirect".into(), Value::from("follow")),
        ("referrer".into(), Value::from("about:client")),
        ("referrerPolicy".into(), Value::from("")),
        ("__w3cos_body".into(), body.clone()),
    ]);
    let clone_input = Value::object(props.clone());
    props.insert(
        "clone".into(),
        Value::function(move |_, _| request_value(clone_input.clone(), Value::Undefined)),
    );
    let text_body = body.clone();
    props.insert(
        "text".into(),
        Value::function(move |_, _| match &text_body {
            Value::Undefined | Value::Null => Value::from(""),
            value => Value::from(value.to_js_string()),
        }),
    );
    let json_body = body;
    props.insert(
        "json".into(),
        Value::function(move |_, _| {
            w3cos_core::json::parse(vec![Value::from(json_body.to_js_string())])
        }),
    );
    Value::object(props)
}

pub fn request_class() -> Value {
    Value::function(|_, args| {
        request_value(
            args.first().cloned().unwrap_or(Value::Undefined),
            args.get(1).cloned().unwrap_or(Value::Undefined),
        )
    })
}

struct AbortState {
    aborted: Cell<bool>,
    reason: RefCell<Value>,
    listeners: RefCell<Vec<Value>>,
}

fn abort_signal_value(state: Rc<AbortState>) -> Value {
    let mut props = HashMap::from([("onabort".into(), Value::Null)]);
    let aborted_state = Rc::clone(&state);
    props.insert(
        "__w3cos_getter_aborted".into(),
        Value::function(move |_, _| Value::Bool(aborted_state.aborted.get())),
    );
    let reason_state = Rc::clone(&state);
    props.insert(
        "__w3cos_getter_reason".into(),
        Value::function(move |_, _| reason_state.reason.borrow().clone()),
    );
    let add_state = Rc::clone(&state);
    props.insert(
        "addEventListener".into(),
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
            let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
            remove_state
                .listeners
                .borrow_mut()
                .retain(|candidate| !candidate.same_value_zero(&listener));
            Value::Undefined
        }),
    );
    Value::object(props)
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
    Value::function(|_, _| {
        let state = Rc::new(AbortState {
            aborted: Cell::new(false),
            reason: RefCell::new(Value::Undefined),
            listeners: RefCell::new(Vec::new()),
        });
        let signal = abort_signal_value(Rc::clone(&state));
        let signal_for_abort = signal.clone();
        Value::object(HashMap::from([
            ("signal".into(), signal),
            (
                "abort".into(),
                Value::function(move |_, args| {
                    let reason = args
                        .first()
                        .cloned()
                        .unwrap_or_else(|| Value::from("AbortError"));
                    abort_state(&state, &signal_for_abort, reason);
                    Value::Undefined
                }),
            ),
        ]))
    })
}

pub fn abort_signal_class() -> Value {
    let class = Value::function(|_, _| Value::Undefined);
    class.set_property(
        "abort",
        Value::function(|_, args| {
            let state = Rc::new(AbortState {
                aborted: Cell::new(false),
                reason: RefCell::new(Value::Undefined),
                listeners: RefCell::new(Vec::new()),
            });
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
    class
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
        let response = response_value(
            FetchResponse {
                status: 200,
                ok: true,
                status_text: "OK".into(),
                headers: HashMap::from([("content-type".into(), "application/json".into())]),
                body_stream: ReadableStream::from_bytes(br#"{"token":"ok"}"#.to_vec()),
            },
            "https://example.test/data".into(),
        );

        assert!(response.get_property("ok").to_bool());
        assert_eq!(response.get_property("status").to_number(), 200.0);
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
                ])),
            ],
        );
        assert_eq!(request.get_property("method").to_js_string(), "POST");
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
