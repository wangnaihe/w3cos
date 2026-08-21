//! Isolated dedicated-worker realms for inline `blob:` / `data:` scripts.
//!
//! Ordinary AOT `Worker` hosts still echo structured-clone messages. This
//! module is compiled only with `dynamic-js`: the parent thread resolves the
//! inline URL (object URLs are thread-local), then the worker OS thread
//! lowers the captured source with SWC → W3IR and runs a fresh W3VM so parent
//! `Value` graphs (`Rc`) never cross the thread boundary.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use w3cos_core::Value;
use w3cos_vm::{Limits, Vm, binding_cell, external_binding_cell};

use crate::worker::{WorkerEvent, WorkerScope};

/// Inline worker URL resolution result.
#[derive(Debug)]
pub(crate) enum InlineWorkerScript {
    Source(String),
    Invalid { message: String },
}

/// Resolve `blob:` / `data:` worker URLs on the parent thread (blob bytes live
/// in that thread's object-URL table). HTTP, file, and dummy URLs return
/// `None` so the existing echo host remains in place.
pub(crate) fn resolve_inline_worker_script(url: &str) -> Option<InlineWorkerScript> {
    if let Some(payload) = url.strip_prefix("data:") {
        return Some(decode_javascript_data_url(payload));
    }
    if url.starts_with("blob:") {
        return Some(decode_blob_worker_url(url));
    }
    None
}

/// Lower and execute `source` on this worker thread, then dispatch `onmessage`.
pub(crate) fn run_dedicated_worker(source: &str, specifier: &str, name: &str, scope: &WorkerScope) {
    if let Err(message) = run_dedicated_worker_inner(source, specifier, name, scope) {
        let _ = scope.report_error(message);
    }
}

fn run_dedicated_worker_inner(
    source: &str,
    specifier: &str,
    name: &str,
    scope: &WorkerScope,
) -> Result<(), String> {
    let module = w3cos_compiler::w3ir_lowering::lower_script(source, specifier)
        .map_err(|error| error.to_string())?;
    let sender = scope.event_sender();
    let post_message = {
        let sender = sender.clone();
        Value::function(move |_, args| {
            let data = args.first().cloned().unwrap_or(Value::Undefined);
            match crate::indexed_db_web::value_to_json(&data) {
                Ok(json) => {
                    let _ = sender.send(WorkerEvent::Message(json));
                }
                Err(error) => w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    &error.message,
                    &error.name,
                )),
            }
            Value::Undefined
        })
    };
    let close = {
        let terminate = scope.terminate_flag();
        Value::function(move |_, _| {
            terminate.store(true, std::sync::atomic::Ordering::SeqCst);
            Value::Undefined
        })
    };
    let self_obj = Value::object(HashMap::from([
        ("name".to_string(), Value::string(name)),
        ("onmessage".to_string(), Value::Null),
        ("onerror".to_string(), Value::Null),
        ("postMessage".to_string(), post_message.clone()),
        ("close".to_string(), close.clone()),
    ]));
    let mut cells = HashMap::new();
    for import in &module.imports {
        if import.specifier != "w3cos:global" {
            return Err(format!(
                "dedicated worker scripts cannot import {}",
                import.specifier
            ));
        }
        let cell = match import.imported.as_str() {
            "onmessage" => {
                let getter_self = self_obj.clone();
                let setter_self = self_obj.clone();
                external_binding_cell(
                    Value::function(move |_, _| getter_self.get_property("onmessage")),
                    Value::function(move |_, args| {
                        setter_self.set_property(
                            "onmessage",
                            args.first().cloned().unwrap_or(Value::Null),
                        );
                        Value::Undefined
                    }),
                )
            }
            imported => binding_cell(resolve_worker_global(
                imported,
                &self_obj,
                &post_message,
                &close,
            )),
        };
        cells.insert(import.local, cell);
    }
    let vm = Vm::new(module, worker_limits()).map_err(|error| error.to_string())?;
    vm.run_with_cells(cells)
        .map_err(|error| error.to_string())?;

    while let Some(message) = scope.recv() {
        let handler = self_obj.get_property("onmessage");
        if !handler.is_callable() {
            continue;
        }
        let data = crate::indexed_db_web::json_to_value(message);
        let event = Value::object(HashMap::from([("data".to_string(), data)]));
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            handler.call(self_obj.clone(), vec![event]);
        }));
        if let Err(payload) = outcome {
            let _ = scope.report_error(panic_message(payload));
        }
    }
    Ok(())
}

fn worker_limits() -> Limits {
    Limits {
        max_instructions: 10_000_000,
        max_call_depth: 512,
        max_heap_bytes: Some(64 * 1024 * 1024),
        max_wall_time: Some(Duration::from_secs(30)),
    }
}

fn resolve_worker_global(
    name: &str,
    self_obj: &Value,
    post_message: &Value,
    close: &Value,
) -> Value {
    match name {
        "self" | "globalThis" => self_obj.clone(),
        "postMessage" => post_message.clone(),
        "close" => close.clone(),
        "undefined" => Value::Undefined,
        "NaN" => Value::Number(f64::NAN),
        "Infinity" => Value::Number(f64::INFINITY),
        "Object" => w3cos_core::object_value(),
        "Array" => w3cos_core::array_value(),
        "Math" => w3cos_core::math_value(),
        "JSON" => w3cos_core::json_value(),
        "console" => console_stub(),
        "Error" => w3cos_core::error_class("Error"),
        "TypeError" => w3cos_core::error_class("TypeError"),
        "RangeError" => w3cos_core::error_class("RangeError"),
        "SyntaxError" => w3cos_core::error_class("SyntaxError"),
        "ReferenceError" => w3cos_core::error_class("ReferenceError"),
        _ => Value::Undefined,
    }
}

fn console_stub() -> Value {
    let log = Value::function(|_, args| {
        let text = args
            .iter()
            .map(Value::to_js_string)
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("[w3cos-worker] {text}");
        Value::Undefined
    });
    Value::object(HashMap::from([
        ("log".to_string(), log.clone()),
        ("info".to_string(), log.clone()),
        ("warn".to_string(), log.clone()),
        ("error".to_string(), log),
        (
            "debug".to_string(),
            Value::function(|_, _| Value::Undefined),
        ),
    ]))
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(value) = payload.downcast_ref::<w3cos_core::PanicValue>() {
        let name = value.0.get_property("name").to_js_string();
        let message = value.0.get_property("message").to_js_string();
        if name.is_empty() {
            return message;
        }
        if message.is_empty() {
            return name;
        }
        return format!("{name}: {message}");
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "Worker script threw".to_string()
}

fn decode_blob_worker_url(url: &str) -> InlineWorkerScript {
    let Some((bytes, mime)) = w3cos_core::web::object_url_resource(url) else {
        return InlineWorkerScript::Invalid {
            message: format!("Failed to load worker script '{url}'"),
        };
    };
    if !is_javascript_mime(&mime) {
        return InlineWorkerScript::Invalid {
            message: format!("Worker blob MIME type '{mime}' is not JavaScript"),
        };
    }
    match String::from_utf8(bytes) {
        Ok(source) => InlineWorkerScript::Source(source),
        Err(_) => InlineWorkerScript::Invalid {
            message: format!("Worker blob '{url}' is not valid UTF-8"),
        },
    }
}

fn decode_javascript_data_url(payload: &str) -> InlineWorkerScript {
    let Some((header, data)) = payload.split_once(',') else {
        return InlineWorkerScript::Invalid {
            message: "Worker data: URL is missing the comma separator".to_string(),
        };
    };
    let mut is_base64 = false;
    let mut mime = String::new();
    for (index, part) in header.split(';').enumerate() {
        let part = part.trim();
        if part.eq_ignore_ascii_case("base64") {
            is_base64 = true;
            continue;
        }
        if index == 0 {
            mime = part.to_string();
            continue;
        }
        if part.to_ascii_lowercase().starts_with("charset=") {
            continue;
        }
    }
    if mime.is_empty() {
        mime = "text/plain".to_string();
    }
    if !is_javascript_mime(&mime) {
        return InlineWorkerScript::Invalid {
            message: format!("Worker data: URL MIME type '{mime}' is not JavaScript"),
        };
    }
    let bytes = if is_base64 {
        let compact: String = data
            .chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect();
        match BASE64.decode(compact.as_bytes()) {
            Ok(bytes) => bytes,
            Err(error) => {
                return InlineWorkerScript::Invalid {
                    message: format!("Worker data: URL base64 decode failed: {error}"),
                };
            }
        }
    } else {
        percent_decode_bytes(data)
    };
    match String::from_utf8(bytes) {
        Ok(source) => InlineWorkerScript::Source(source),
        Err(_) => InlineWorkerScript::Invalid {
            message: "Worker data: URL payload is not valid UTF-8".to_string(),
        },
    }
}

fn is_javascript_mime(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    matches!(
        mime.as_str(),
        "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "text/jscript"
    )
}

fn percent_decode_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(&hi), Some(&lo)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                if let (Some(hi), Some(lo)) = (from_hex(hi), from_hex(lo)) {
                    out.push(hi * 16 + lo);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_plus_is_not_turned_into_space() {
        let script = resolve_inline_worker_script(
            "data:text/javascript,self.onmessage=function(event){postMessage(event.data+1);}",
        );
        match script {
            Some(InlineWorkerScript::Source(source)) => {
                assert!(source.contains("event.data+1"), "{source}");
            }
            other => panic!("expected javascript source, got {other:?}"),
        }
    }

    #[test]
    fn http_worker_urls_stay_on_the_echo_host() {
        assert!(resolve_inline_worker_script("https://example.test/worker.js").is_none());
        assert!(resolve_inline_worker_script("echo").is_none());
    }
}
