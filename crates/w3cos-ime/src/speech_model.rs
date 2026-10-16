//! Optional on-device Whisper weights.
//!
//! The model is not bundled. [`download_speech_model`] is the only function
//! that opens a socket, and only after `consent` is true. It checks the pinned
//! SHA-256 before the file is renamed into place. Recognition never calls it.
//!
//! Weights: `ggml-tiny-q5_1.bin`, the multilingual Whisper tiny model quantized
//! by whisper.cpp (MIT). Chinese and English share this one file. English-only
//! `*.en` weights are not used.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// File name inside the speech directory.
pub const MODEL_FILE: &str = "ggml-tiny-q5_1.bin";

/// Byte length of the pinned file. A longer body is discarded before install.
pub const MODEL_LEN: usize = 32_152_673;

/// SHA-256 of [`MODEL_FILE`] from the whisper.cpp Hugging Face repo.
pub const MODEL_SHA256: &str = "818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7";

/// Pinned download. Redirects stay on this host's published object.
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny-q5_1.bin";

#[derive(Debug, PartialEq, Eq)]
pub enum ModelError {
    /// The person did not agree. No socket is opened.
    ConsentRequired,
    ChecksumMismatch {
        expected: String,
        actual: String,
    },
    TooLarge,
    Http(String),
    Io(String),
}

struct Memo {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
    head: [u8; 16],
    ready: bool,
}

static MEMO: Mutex<Option<Memo>> = Mutex::new(None);

/// Where a verified model is stored.
///
/// `W3COS_SPEECH_DIR` wins. Otherwise `$XDG_DATA_HOME/w3cos/speech` or
/// `~/.local/share/w3cos/speech`.
pub fn default_speech_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("W3COS_SPEECH_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("w3cos").join("speech")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        out.push(char::from(b"0123456789abcdef"[(byte & 0xf) as usize]));
    }
    out
}

/// `true` only when `MODEL_FILE` is present and its bytes match the pin.
pub fn model_ready(dir: &Path) -> bool {
    file_matches(&dir.join(MODEL_FILE), MODEL_SHA256, MODEL_LEN as u64)
}

fn file_matches(path: &Path, expected_sha: &str, expected_len: u64) -> bool {
    let meta = match fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return false,
    };
    if meta.len() != expected_len {
        return false;
    }
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mut head = [0u8; 16];
    if let Ok(mut file) = fs::File::open(path) {
        let _ = file.read(&mut head);
    }
    let mut memo = MEMO.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(cached) = memo.as_ref()
        && cached.path == path
        && cached.modified == modified
        && cached.len == expected_len
        && cached.head == head
    {
        return cached.ready;
    }
    let bytes = fs::read(path).unwrap_or_default();
    let ready = bytes.len() as u64 == expected_len && sha256_hex(&bytes) == expected_sha;
    let mut stored_head = [0u8; 16];
    let n = bytes.len().min(stored_head.len());
    stored_head[..n].copy_from_slice(&bytes[..n]);
    *memo = Some(Memo {
        path: path.to_path_buf(),
        modified,
        len: expected_len,
        head: stored_head,
        ready,
    });
    ready
}

/// Write `bytes` only when they hash to `expected_sha256`.
///
/// This does not open a socket. A mismatch leaves the directory unchanged.
pub fn install_checked(
    dir: &Path,
    bytes: &[u8],
    expected_sha256: &str,
) -> Result<PathBuf, ModelError> {
    let actual = sha256_hex(bytes);
    if actual != expected_sha256 {
        return Err(ModelError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    fs::create_dir_all(dir).map_err(|error| ModelError::Io(error.to_string()))?;
    let partial = dir.join(format!("{MODEL_FILE}.partial"));
    let final_path = dir.join(MODEL_FILE);
    fs::write(&partial, bytes).map_err(|error| ModelError::Io(error.to_string()))?;
    fs::rename(&partial, &final_path).map_err(|error| {
        let _ = fs::remove_file(&partial);
        ModelError::Io(error.to_string())
    })?;
    Ok(final_path)
}

/// Download the pinned model. `consent == false` returns before any connection.
pub fn download_speech_model(dir: &Path, consent: bool) -> Result<PathBuf, ModelError> {
    download_from(dir, consent, MODEL_URL, MODEL_SHA256, MODEL_LEN)
}

/// `max_len` caps the body. The pinned model passes [`MODEL_LEN`].
pub fn download_from(
    dir: &Path,
    consent: bool,
    url: &str,
    expected_sha256: &str,
    max_len: usize,
) -> Result<PathBuf, ModelError> {
    if !consent {
        return Err(ModelError::ConsentRequired);
    }
    let bytes = http_get(url, max_len)?;
    install_checked(dir, &bytes, expected_sha256)
}

fn http_get(url: &str, max_len: usize) -> Result<Vec<u8>, ModelError> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = agent
        .get(url)
        .call()
        .map_err(|error| ModelError::Http(error.to_string()))?;
    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|error| ModelError::Io(error.to_string()))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > max_len {
            return Err(ModelError::TooLarge);
        }
        bytes.extend_from_slice(&buf[..read]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn scratch() -> PathBuf {
        let mut nonce = [0u8; 8];
        getrandom::getrandom(&mut nonce).unwrap();
        let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("w3cos-speech-{name}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pinned_model_identity_is_stable() {
        assert_eq!(MODEL_SHA256.len(), 64);
        assert!(MODEL_SHA256.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(MODEL_URL.ends_with(MODEL_FILE));
        assert!(MODEL_URL.starts_with("https://"));
        assert!(MODEL_LEN > 1_000_000);
    }

    #[test]
    fn missing_model_is_not_ready_and_a_mismatch_is_not_installed() {
        let dir = TempDir(scratch());
        assert!(!model_ready(&dir.0));
        let err = install_checked(&dir.0, b"not-the-model", MODEL_SHA256).unwrap_err();
        assert!(matches!(err, ModelError::ChecksumMismatch { .. }));
        assert!(!dir.0.join(MODEL_FILE).exists());
        assert!(!dir.0.join(format!("{MODEL_FILE}.partial")).exists());

        let payload = b"hello-model";
        let hash = sha256_hex(payload);
        let path = install_checked(&dir.0, payload, &hash).unwrap();
        assert_eq!(fs::read(&path).unwrap(), payload);
        assert!(
            !model_ready(&dir.0),
            "a stand-in file is not the pinned model"
        );
    }

    #[test]
    fn a_changed_file_stops_matching() {
        let dir = TempDir(scratch());
        let payload = b"hello-model-bytes";
        let hash = sha256_hex(payload);
        let path = dir.0.join("stand-in.bin");
        fs::write(&path, payload).unwrap();
        assert!(file_matches(&path, &hash, payload.len() as u64));
        assert!(file_matches(&path, &hash, payload.len() as u64));
        let mut flipped = payload.to_vec();
        flipped[0] ^= 0xff;
        fs::write(&path, &flipped).unwrap();
        assert!(!file_matches(&path, &hash, payload.len() as u64));
    }

    fn serve(body: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(header.as_bytes());
            let _ = sock.write_all(body);
        });
        port
    }

    #[test]
    fn refused_consent_opens_no_socket_and_a_bad_body_is_dropped() {
        let dir = TempDir(scratch());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/ggml-tiny-q5_1.bin");
        let err = download_from(&dir.0, false, &url, MODEL_SHA256, MODEL_LEN).unwrap_err();
        assert_eq!(err, ModelError::ConsentRequired);
        assert!(listener.accept().is_err(), "consent false still connected");
        assert!(!dir.0.join(MODEL_FILE).exists());

        let port = serve(b"nope");
        let url = format!("http://127.0.0.1:{port}/model.bin");
        let err = download_from(&dir.0, true, &url, MODEL_SHA256, 64).unwrap_err();
        assert!(
            matches!(err, ModelError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert!(!dir.0.join(MODEL_FILE).exists());

        let port = serve(b"hello-model");
        let url = format!("http://127.0.0.1:{port}/model.bin");
        let hash = sha256_hex(b"hello-model");
        let path = download_from(&dir.0, true, &url, &hash, 64).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"hello-model");
        assert!(!model_ready(&dir.0));
    }
}
