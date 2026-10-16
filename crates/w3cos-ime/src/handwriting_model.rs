//! Optional on-device handwriting weights.
//!
//! The model is not bundled. [`download_handwriting_model`] is the only
//! function that opens a socket, and only after `consent` is true. It checks
//! both pinned SHA-256 values before either file is renamed into place.
//! Recognition never calls it.
//!
//! Weights: PaddleOCR `PP-OCRv5_mobile_rec` (Apache-2.0), the mobile text-line
//! recognizer. One file covers simplified Chinese, traditional Chinese,
//! English, and Japanese, including handwriting. The companion
//! `inference.yml` is the character list from the same official export.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::speech_model::sha256_hex;

/// ONNX file name inside the handwriting directory.
pub const INK_MODEL_FILE: &str = "inference.onnx";

/// Byte length of the pinned ONNX file.
pub const INK_MODEL_LEN: usize = 16_534_782;

/// SHA-256 of [`INK_MODEL_FILE`] from the official PaddlePaddle export.
pub const INK_MODEL_SHA256: &str =
    "da72dc72ca4dc220df0dfde68c1dedc31c58d3e76a25871122e5056227d50092";

/// Pinned ONNX download.
pub const INK_MODEL_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv5_mobile_rec_onnx/resolve/main/inference.onnx";

/// Character-list file that belongs to the same export.
pub const DICT_FILE: &str = "inference.yml";

/// Byte length of the pinned character list.
pub const DICT_LEN: usize = 148_345;

/// SHA-256 of [`DICT_FILE`].
pub const DICT_SHA256: &str = "5dfeb2777f6d0db8177d8128a8acfcf6e6276dc4ac73ea3bf0dc06d6a5e85d8e";

/// Pinned character-list download.
pub const DICT_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv5_mobile_rec_onnx/resolve/main/inference.yml";

#[derive(Debug, PartialEq, Eq)]
pub enum InkModelError {
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
    modified: SystemTime,
    len: u64,
    head: [u8; 16],
    ready: bool,
}

static MEMO: Mutex<Option<HashMap<PathBuf, Memo>>> = Mutex::new(None);

/// Where verified handwriting files are stored.
///
/// `W3COS_HANDWRITING_DIR` wins. Otherwise `$XDG_DATA_HOME/w3cos/handwriting`
/// or `~/.local/share/w3cos/handwriting`.
pub fn default_handwriting_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("W3COS_HANDWRITING_DIR") {
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
    base.join("w3cos").join("handwriting")
}

/// `true` only when both pinned files are present and match.
pub fn handwriting_model_ready(dir: &Path) -> bool {
    file_matches(
        &dir.join(INK_MODEL_FILE),
        INK_MODEL_SHA256,
        INK_MODEL_LEN as u64,
    ) && file_matches(&dir.join(DICT_FILE), DICT_SHA256, DICT_LEN as u64)
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
    let map = memo.get_or_insert_with(HashMap::new);
    if let Some(cached) = map.get(path)
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
    map.insert(
        path.to_path_buf(),
        Memo {
            modified,
            len: expected_len,
            head: stored_head,
            ready,
        },
    );
    ready
}

fn install_named(
    dir: &Path,
    file_name: &str,
    bytes: &[u8],
    expected_sha256: &str,
) -> Result<PathBuf, InkModelError> {
    let actual = sha256_hex(bytes);
    if actual != expected_sha256 {
        return Err(InkModelError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    fs::create_dir_all(dir).map_err(|error| InkModelError::Io(error.to_string()))?;
    let partial = dir.join(format!("{file_name}.partial"));
    let final_path = dir.join(file_name);
    fs::write(&partial, bytes).map_err(|error| InkModelError::Io(error.to_string()))?;
    fs::rename(&partial, &final_path).map_err(|error| {
        let _ = fs::remove_file(&partial);
        InkModelError::Io(error.to_string())
    })?;
    Ok(final_path)
}

/// Download both pinned files. `consent == false` returns before any connection.
pub fn download_handwriting_model(dir: &Path, consent: bool) -> Result<PathBuf, InkModelError> {
    download_pair(
        dir,
        consent,
        INK_MODEL_URL,
        INK_MODEL_SHA256,
        INK_MODEL_LEN,
        INK_MODEL_FILE,
        DICT_URL,
        DICT_SHA256,
        DICT_LEN,
        DICT_FILE,
    )
}

/// Fetch two bodies and install them only when both hashes match.
#[allow(clippy::too_many_arguments)]
pub fn download_pair(
    dir: &Path,
    consent: bool,
    model_url: &str,
    model_sha: &str,
    model_len: usize,
    model_name: &str,
    dict_url: &str,
    dict_sha: &str,
    dict_len: usize,
    dict_name: &str,
) -> Result<PathBuf, InkModelError> {
    if !consent {
        return Err(InkModelError::ConsentRequired);
    }
    let dict = http_get(dict_url, dict_len)?;
    let dict_actual = sha256_hex(&dict);
    if dict_actual != dict_sha {
        return Err(InkModelError::ChecksumMismatch {
            expected: dict_sha.to_string(),
            actual: dict_actual,
        });
    }
    let model = http_get(model_url, model_len)?;
    let model_actual = sha256_hex(&model);
    if model_actual != model_sha {
        return Err(InkModelError::ChecksumMismatch {
            expected: model_sha.to_string(),
            actual: model_actual,
        });
    }
    install_named(dir, dict_name, &dict, dict_sha)?;
    match install_named(dir, model_name, &model, model_sha) {
        Ok(path) => Ok(path),
        Err(error) => {
            let _ = fs::remove_file(dir.join(dict_name));
            let _ = fs::remove_file(dir.join(model_name));
            Err(error)
        }
    }
}

fn http_get(url: &str, max_len: usize) -> Result<Vec<u8>, InkModelError> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = agent
        .get(url)
        .call()
        .map_err(|error| InkModelError::Http(error.to_string()))?;
    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|error| InkModelError::Io(error.to_string()))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > max_len {
            return Err(InkModelError::TooLarge);
        }
        bytes.extend_from_slice(&buf[..read]);
    }
    Ok(bytes)
}

/// Lines under `character_dict:` in the PaddleX export. Blank is not included.
pub fn parse_character_dict(yml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut on = false;
    for line in yml.lines() {
        if !on {
            if line.trim() == "character_dict:" {
                on = true;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("  - ") {
            if !rest.is_empty() {
                out.push(rest.to_string());
            }
        } else if !line.trim().is_empty() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    fn scratch() -> PathBuf {
        let mut nonce = [0u8; 8];
        getrandom::getrandom(&mut nonce).unwrap();
        let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("w3cos-ink-{name}"));
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
        assert_eq!(INK_MODEL_SHA256.len(), 64);
        assert_eq!(DICT_SHA256.len(), 64);
        assert!(INK_MODEL_SHA256.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(DICT_SHA256.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(INK_MODEL_URL.ends_with(INK_MODEL_FILE));
        assert!(DICT_URL.ends_with(DICT_FILE));
        assert!(INK_MODEL_URL.starts_with("https://huggingface.co/PaddlePaddle/"));
        const _: () = assert!(INK_MODEL_LEN > 1_000_000);
        const _: () = assert!(DICT_LEN > 1_000);
    }

    #[test]
    fn character_dict_reads_the_export_list() {
        let yml = "PostProcess:\n  name: CTCLabelDecode\n  character_dict:\n  - 你\n  - 好\nOther:\n  - no\n";
        assert_eq!(
            parse_character_dict(yml),
            vec!["你".to_string(), "好".to_string()]
        );
    }

    #[test]
    fn missing_model_is_not_ready_and_a_mismatch_is_not_installed() {
        let dir = TempDir(scratch());
        assert!(!handwriting_model_ready(&dir.0));
        let err =
            install_named(&dir.0, INK_MODEL_FILE, b"not-the-model", INK_MODEL_SHA256).unwrap_err();
        assert!(matches!(err, InkModelError::ChecksumMismatch { .. }));
        assert!(!dir.0.join(INK_MODEL_FILE).exists());
        assert!(!dir.0.join(format!("{INK_MODEL_FILE}.partial")).exists());
    }

    #[test]
    fn a_changed_file_stops_matching() {
        let dir = TempDir(scratch());
        let payload = b"hello-ink-bytes!!";
        let hash = sha256_hex(payload);
        let path = dir.0.join("stand-in.bin");
        fs::write(&path, payload).unwrap();
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
        let url = format!("http://127.0.0.1:{port}/inference.onnx");
        let err = download_pair(
            &dir.0,
            false,
            &url,
            INK_MODEL_SHA256,
            INK_MODEL_LEN,
            INK_MODEL_FILE,
            &url,
            DICT_SHA256,
            DICT_LEN,
            DICT_FILE,
        )
        .unwrap_err();
        assert_eq!(err, InkModelError::ConsentRequired);
        assert!(listener.accept().is_err(), "consent false still connected");
        assert!(!dir.0.join(INK_MODEL_FILE).exists());
        assert!(!dir.0.join(DICT_FILE).exists());

        let dict_port = serve(b"nope-dict");
        let model_port = serve(b"nope-model");
        let err = download_pair(
            &dir.0,
            true,
            &format!("http://127.0.0.1:{model_port}/inference.onnx"),
            INK_MODEL_SHA256,
            64,
            INK_MODEL_FILE,
            &format!("http://127.0.0.1:{dict_port}/inference.yml"),
            DICT_SHA256,
            64,
            DICT_FILE,
        )
        .unwrap_err();
        assert!(
            matches!(err, InkModelError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert!(!dir.0.join(INK_MODEL_FILE).exists());
        assert!(!dir.0.join(DICT_FILE).exists());

        let dict = b"dict-body";
        let model = b"model-body";
        let dict_port = serve(dict);
        let model_port = serve(model);
        let path = download_pair(
            &dir.0,
            true,
            &format!("http://127.0.0.1:{model_port}/inference.onnx"),
            &sha256_hex(model),
            64,
            INK_MODEL_FILE,
            &format!("http://127.0.0.1:{dict_port}/inference.yml"),
            &sha256_hex(dict),
            64,
            DICT_FILE,
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), model);
        assert_eq!(fs::read(dir.0.join(DICT_FILE)).unwrap(), dict);
        assert!(!handwriting_model_ready(&dir.0));
    }
}
