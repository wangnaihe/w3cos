//! On-device weight file for the opt-in in-process engine.
//!
//! [`download_model`] is the only function here that opens a socket, and only
//! after `consent` is true. The bytes are hashed before the file is renamed
//! into place. Completion never calls it. `w3cos-inferd` does not use this
//! path: that process is started by the operator, outside the shell.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ureq::Agent;

/// File name inside the inference directory.
pub const MODEL_FILE: &str = "in-process.w3iw";

#[derive(Debug, PartialEq, Eq)]
pub enum ModelError {
    /// The person did not agree. No socket is opened.
    ConsentRequired,
    /// Consent was given, but no URL was configured. No socket is opened.
    UrlUnset,
    /// Consent was given, but no SHA-256 pin was configured. No socket is opened.
    PinUnset,
    ChecksumMismatch {
        expected: String,
        actual: String,
    },
    TooLarge,
    Http(String),
    Io(String),
}

/// Where a verified in-process model is stored.
///
/// `W3COS_INFER_DIR` wins. Otherwise `$XDG_DATA_HOME/w3cos/infer` or
/// `~/.local/share/w3cos/infer`.
#[cfg_attr(not(feature = "in-process-infer"), allow(dead_code))]
pub fn default_infer_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("W3COS_INFER_DIR") {
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
    base.join("w3cos").join("infer")
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

/// `true` only when [`MODEL_FILE`] is present, the length matches, and the
/// SHA-256 matches `expected_sha`.
pub fn model_ready(dir: &Path, expected_sha: &str, expected_len: u64) -> bool {
    file_matches(&dir.join(MODEL_FILE), expected_sha, expected_len)
}

pub fn file_matches(path: &Path, expected_sha: &str, expected_len: u64) -> bool {
    let meta = match fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return false,
    };
    if meta.len() != expected_len {
        return false;
    }
    let bytes = fs::read(path).unwrap_or_default();
    bytes.len() as u64 == expected_len && sha256_hex(&bytes) == normalize_sha(expected_sha)
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
    let expected = normalize_sha(expected_sha256);
    if actual != expected {
        return Err(ModelError::ChecksumMismatch { expected, actual });
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

/// Download the pinned model. `consent == false` returns before any connection
/// and before the URL or the pin is read.
pub fn download_model(dir: &Path, consent: bool) -> Result<PathBuf, ModelError> {
    if !consent {
        return Err(ModelError::ConsentRequired);
    }
    let url = std::env::var("W3COS_INFER_MODEL_URL").unwrap_or_default();
    let url = url.trim();
    if url.is_empty() {
        return Err(ModelError::UrlUnset);
    }
    let sha = std::env::var("W3COS_INFER_SHA256").unwrap_or_default();
    let sha = sha.trim();
    if sha.is_empty() {
        return Err(ModelError::PinUnset);
    }
    let max_len = std::env::var("W3COS_INFER_LEN")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(1024 * 1024);
    download_from(dir, true, url, sha, max_len)
}

/// `max_len` caps the body. The pin is checked before the file is installed.
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

fn normalize_sha(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn http_get(url: &str, max_len: usize) -> Result<Vec<u8>, ModelError> {
    let agent = Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .max_redirects(0)
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| ModelError::Http(error.to_string()))?;
    let mut reader = response.body_mut().as_reader();
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
        getrandom::fill(&mut nonce).unwrap();
        let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("w3cos-infer-{name}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
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
        let url = format!("http://127.0.0.1:{port}/in-process.w3iw");
        let err = download_from(&dir.0, false, &url, "abc", 64).unwrap_err();
        assert_eq!(err, ModelError::ConsentRequired);
        assert!(listener.accept().is_err(), "consent false still connected");
        assert!(!dir.0.join(MODEL_FILE).exists());

        let kept = dir.0.join(MODEL_FILE);
        fs::write(&kept, b"kept").unwrap();
        let err = download_model(&dir.0, false).unwrap_err();
        assert_eq!(err, ModelError::ConsentRequired);
        assert_eq!(fs::read(&kept).unwrap(), b"kept");

        let port = serve(b"nope");
        let url = format!("http://127.0.0.1:{port}/in-process.w3iw");
        let err = download_from(&dir.0, true, &url, &sha256_hex(b"other"), 64).unwrap_err();
        assert!(
            matches!(err, ModelError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert_eq!(fs::read(&kept).unwrap(), b"kept");

        let port = serve(b"hello-model");
        let url = format!("http://127.0.0.1:{port}/in-process.w3iw");
        let hash = sha256_hex(b"hello-model");
        let path = download_from(&dir.0, true, &url, &hash, 64).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello-model");
        assert!(model_ready(&dir.0, &hash, b"hello-model".len() as u64));
        assert!(!model_ready(
            &dir.0,
            &sha256_hex(b"other"),
            b"hello-model".len() as u64
        ));
    }

    #[test]
    fn oversized_body_is_dropped_before_install() {
        let dir = TempDir(scratch());
        let port = serve(b"0123456789");
        let url = format!("http://127.0.0.1:{port}/in-process.w3iw");
        let err = download_from(&dir.0, true, &url, &sha256_hex(b"0123456789"), 4).unwrap_err();
        assert_eq!(err, ModelError::TooLarge);
        assert!(!dir.0.join(MODEL_FILE).exists());
        assert!(!dir.0.join(format!("{MODEL_FILE}.partial")).exists());
    }
}
