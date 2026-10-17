//! Inference backends.
//!
//! [`InferenceBackend`] is the completion surface. [`InferdBackend`] talks to
//! `w3cos-inferd` (or any OpenAI-compatible server) and is what runs when
//! `W3COS_INFER_BACKEND` is unset. [`InProcessBackend`](engine::InProcessBackend)
//! is compiled with the `in-process-infer` feature and runs only when the
//! runtime switch asks for it. Callers still use [`crate::planner::Planner`].

use std::sync::Mutex;

use crate::planner::{OpenAiConfig, OpenAiPlanner, PlannerCompletion, PlannerTurn, post_chat};

#[cfg(feature = "in-process-infer")]
mod engine;
mod model;

pub use model::{
    MODEL_FILE, ModelError, download_from, download_model, install_checked, model_ready, sha256_hex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InferBackendKind {
    /// Separate process. This is the default.
    Inferd,
    /// Inside this process. Requires the `in-process-infer` feature.
    InProcess,
}

impl InferBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inferd => "inferd",
            Self::InProcess => "in-process",
        }
    }
}

/// Completion surface shared by `w3cos-inferd` and the opt-in in-process engine.
pub trait InferenceBackend: Send {
    fn kind(&self) -> InferBackendKind;
    fn complete(&self, turn: &PlannerTurn) -> Result<PlannerCompletion, String>;
}

pub struct InferdBackend {
    pub config: OpenAiConfig,
}

impl InferenceBackend for InferdBackend {
    fn kind(&self) -> InferBackendKind {
        InferBackendKind::Inferd
    }

    fn complete(&self, turn: &PlannerTurn) -> Result<PlannerCompletion, String> {
        let started = std::time::Instant::now();
        let body = OpenAiPlanner {
            config: self.config.clone(),
        }
        .request_body(turn);
        let (bytes, first_token_ms) = post_chat(&self.config, &body)?;
        let text = String::from_utf8_lossy(&bytes);
        let step = crate::planner::parse_model_body(&text)?;
        Ok(PlannerCompletion {
            step,
            first_token_ms,
            total_ms: started.elapsed().as_millis(),
        })
    }
}

#[cfg(feature = "in-process-infer")]
struct InProcessAdapter {
    inner: engine::InProcessBackend,
}

#[cfg(feature = "in-process-infer")]
impl InferenceBackend for InProcessAdapter {
    fn kind(&self) -> InferBackendKind {
        InferBackendKind::InProcess
    }

    fn complete(&self, turn: &PlannerTurn) -> Result<PlannerCompletion, String> {
        self.inner.complete_turn(turn)
    }
}

static RUNTIME_OVERRIDE: Mutex<Option<InferBackendKind>> = Mutex::new(None);

fn override_slot() -> std::sync::MutexGuard<'static, Option<InferBackendKind>> {
    RUNTIME_OVERRIDE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// `true` when this binary was built with `in-process-infer`.
pub fn in_process_compiled() -> bool {
    cfg!(feature = "in-process-infer")
}

/// Parse a runtime value. Anything other than `in-process` stays on inferd.
pub fn backend_from_value(value: &str) -> InferBackendKind {
    match value.trim() {
        "in-process" | "inprocess" => InferBackendKind::InProcess,
        _ => InferBackendKind::Inferd,
    }
}

/// Settings switch, else `W3COS_INFER_BACKEND`. Unset means inferd.
pub fn requested_backend() -> InferBackendKind {
    if let Some(kind) = *override_slot() {
        return kind;
    }
    backend_from_value(&std::env::var("W3COS_INFER_BACKEND").unwrap_or_default())
}

/// The backend that will actually run. A request for in-process on a build
/// that does not include the feature stays on inferd.
pub fn active_backend() -> InferBackendKind {
    match requested_backend() {
        InferBackendKind::InProcess if in_process_compiled() => InferBackendKind::InProcess,
        _ => InferBackendKind::Inferd,
    }
}

/// Session switch for the settings page. Does not download weights.
///
/// In-process is refused when this binary was built without the feature, and
/// the stored choice stays where it was.
pub fn set_runtime_backend(kind: InferBackendKind) -> Result<(), &'static str> {
    if kind == InferBackendKind::InProcess && !in_process_compiled() {
        return Err("in-process inference was not compiled into this build");
    }
    *override_slot() = Some(kind);
    Ok(())
}

pub fn clear_runtime_backend() {
    *override_slot() = None;
}

/// Backend for one planner step. Callers of [`crate::planner::Planner`] do
/// not construct this themselves.
pub fn backend_for(config: &OpenAiConfig) -> Result<Box<dyn InferenceBackend>, String> {
    match requested_backend() {
        InferBackendKind::Inferd => Ok(Box::new(InferdBackend {
            config: config.clone(),
        })),
        InferBackendKind::InProcess => in_process_boxed(),
    }
}

#[cfg(feature = "in-process-infer")]
fn in_process_boxed() -> Result<Box<dyn InferenceBackend>, String> {
    Ok(Box::new(InProcessAdapter {
        inner: engine::InProcessBackend::load_configured()?,
    }))
}

#[cfg(not(feature = "in-process-infer"))]
fn in_process_boxed() -> Result<Box<dyn InferenceBackend>, String> {
    Err("in-process inference was not compiled into this build".into())
}

pub fn dispatch(config: &OpenAiConfig, turn: &PlannerTurn) -> Result<PlannerCompletion, String> {
    backend_for(config)?.complete(turn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{ChatMessage, PlannerStep};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    static LOCK: Mutex<()> = Mutex::new(());

    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            clear_runtime_backend();
            unsafe {
                std::env::remove_var("W3COS_INFER_BACKEND");
                std::env::remove_var("W3COS_L2");
                std::env::remove_var("W3COS_L2_BASE_URL");
                std::env::remove_var("W3COS_INFER_DIR");
                std::env::remove_var("W3COS_INFER_SHA256");
                std::env::remove_var("W3COS_INFER_LEN");
                std::env::remove_var("W3COS_INFER_MODEL_URL");
            }
        }
    }

    fn lock() -> (std::sync::MutexGuard<'static, ()>, Reset) {
        let guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_runtime_backend();
        unsafe {
            std::env::remove_var("W3COS_INFER_BACKEND");
        }
        (guard, Reset)
    }

    fn sample_config(base_url: &str) -> OpenAiConfig {
        OpenAiConfig {
            base_url: base_url.into(),
            model: "local".into(),
            api_key: None,
            timeout: Duration::from_secs(2),
        }
    }

    #[test]
    fn unset_backend_is_inferd_even_before_a_feature_choice() {
        let (_guard, _reset) = lock();
        assert_eq!(backend_from_value(""), InferBackendKind::Inferd);
        assert_eq!(backend_from_value("inferd"), InferBackendKind::Inferd);
        assert_eq!(backend_from_value("w3cos-inferd"), InferBackendKind::Inferd);
        assert_eq!(
            backend_from_value("in-process"),
            InferBackendKind::InProcess
        );
        assert_eq!(requested_backend(), InferBackendKind::Inferd);
        assert_eq!(active_backend(), InferBackendKind::Inferd);
        let backend = backend_for(&sample_config("http://127.0.0.1:9/v1")).unwrap();
        assert_eq!(backend.kind(), InferBackendKind::Inferd);
    }

    #[test]
    fn inferd_backend_posts_through_the_same_trait() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
            read_http_request(&mut sock);
            let body = b"{\"choices\":[{\"message\":{\"content\":\"done\"}}]}";
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(header.as_bytes());
            let _ = sock.write_all(body);
        });
        let backend = InferdBackend {
            config: sample_config(&format!("http://127.0.0.1:{port}/v1")),
        };
        let turn = PlannerTurn {
            messages: vec![ChatMessage::user("open files")],
        };
        let completion = run_backend(&backend, &turn).unwrap();
        match completion.step {
            PlannerStep::Final(text) => assert_eq!(text, "done"),
            PlannerStep::Calls(calls) => panic!("{calls:?}"),
        }
    }

    fn read_http_request(sock: &mut std::net::TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        while let Ok(read) = sock.read(&mut chunk) {
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
                if buf.len() > 1024 * 1024 {
                    break;
                }
                continue;
            };
            let headers = String::from_utf8_lossy(&buf[..end]);
            let length = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            });
            let have = buf.len() - (end + 4);
            if have >= length.unwrap_or(0) || buf.len() > 1024 * 1024 {
                break;
            }
        }
    }

    fn run_backend(
        backend: &dyn InferenceBackend,
        turn: &PlannerTurn,
    ) -> Result<PlannerCompletion, String> {
        assert_eq!(backend.kind(), InferBackendKind::Inferd);
        backend.complete(turn)
    }

    #[cfg(not(feature = "in-process-infer"))]
    #[test]
    fn in_process_request_without_the_feature_does_not_load_weights() {
        let (_guard, _reset) = lock();
        unsafe {
            std::env::set_var("W3COS_INFER_BACKEND", "in-process");
            std::env::set_var("W3COS_L2", "1");
        }
        assert!(!in_process_compiled());
        assert_eq!(requested_backend(), InferBackendKind::InProcess);
        assert_eq!(active_backend(), InferBackendKind::Inferd);
        assert!(set_runtime_backend(InferBackendKind::InProcess).is_err());
        assert_eq!(requested_backend(), InferBackendKind::InProcess);
        let err = match backend_for(&sample_config("http://127.0.0.1:9/v1")) {
            Err(err) => err,
            Ok(_) => panic!("in-process request built a backend"),
        };
        assert!(err.contains("not compiled"), "{err}");
        let mode = crate::planner::mode_from_env().unwrap_err();
        assert!(mode.contains("in-process"), "{mode}");
    }

    #[cfg(feature = "in-process-infer")]
    #[test]
    fn feature_build_stays_on_inferd_until_the_runtime_switch() {
        use crate::planner::Planner;
        let (_guard, _reset) = lock();
        assert!(in_process_compiled());
        assert_eq!(active_backend(), InferBackendKind::Inferd);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("w3cos-infer-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = super::engine::encode_weights(
            8,
            &[
                super::engine::WeightRow {
                    bias: 0,
                    name: "propose_intent".into(),
                    arguments: r#"{"utterance":"open files"}"#.into(),
                    weights: vec![0, 0, 0, 0, 0, 0, 10, 0],
                },
                super::engine::WeightRow {
                    bias: 1,
                    name: "final".into(),
                    arguments: "No shell command matched this request.".into(),
                    weights: vec![0; 8],
                },
            ],
        )
        .unwrap();
        let sha = sha256_hex(&bytes);
        install_checked(&dir, &bytes, &sha).unwrap();
        unsafe {
            std::env::set_var("W3COS_INFER_DIR", &dir);
            std::env::set_var("W3COS_INFER_SHA256", &sha);
            std::env::set_var("W3COS_INFER_LEN", bytes.len().to_string());
            std::env::set_var("W3COS_L2", "1");
            std::env::remove_var("W3COS_L2_BASE_URL");
        }
        assert!(set_runtime_backend(InferBackendKind::InProcess).is_ok());
        assert_eq!(active_backend(), InferBackendKind::InProcess);
        let mode = crate::planner::mode_from_env().unwrap();
        let config = match mode {
            crate::planner::L2Mode::Remote(config) => config,
            other => panic!("{other:?}"),
        };
        assert!(config.base_url.is_empty());
        let turn = PlannerTurn {
            messages: vec![ChatMessage::user("please open the files application")],
        };
        let planner = OpenAiPlanner {
            config: config.clone(),
        };
        match planner.complete(&turn).unwrap().step {
            PlannerStep::Calls(calls) => assert_eq!(calls[0].name, "propose_intent"),
            PlannerStep::Final(text) => panic!("{text}"),
        }
        assert!(
            listener.accept().is_err(),
            "in-process completion opened a socket"
        );

        set_runtime_backend(InferBackendKind::Inferd).unwrap();
        assert_eq!(active_backend(), InferBackendKind::Inferd);
        let backend = backend_for(&sample_config(&format!("http://127.0.0.1:{port}/v1"))).unwrap();
        assert_eq!(backend.kind(), InferBackendKind::Inferd);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
