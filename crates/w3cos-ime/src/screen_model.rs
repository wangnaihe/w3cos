//! Optional on-device screen-text weights.
//!
//! The detector is not bundled. [`download_screen_text_model`] is the only
//! function here that opens a socket, and only after `consent` is true. It
//! checks the pinned SHA-256 values before either detector file is renamed
//! into place. Recognition never calls it.
//!
//! The recognizer is the handwriting model's PP-OCRv5 mobile rec. This
//! download adds the official PP-OCRv5 mobile det ONNX (Apache-2.0). If the
//! recognizer is not already verified, the same consent also fetches that
//! pair into the handwriting directory. A mismatch discards the new files.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::handwriting_model::{self, InkModelError, download_pair, handwriting_model_ready};
use crate::speech_model::sha256_hex;

/// Detector ONNX file name inside the screen-text directory.
pub const DET_FILE: &str = "inference.onnx";

/// Byte length of the pinned detector.
pub const DET_LEN: usize = 4_826_518;

/// SHA-256 of [`DET_FILE`] from the official PaddlePaddle export.
pub const DET_SHA256: &str = "a431985659dc921974177a95adcfbb90fd9e51989a5e04d70d0b75f597b6e61d";

/// Pinned detector download.
pub const DET_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv5_mobile_det_onnx/resolve/main/inference.onnx";

/// Detector config that belongs to the same export.
pub const DET_YML: &str = "inference.yml";

/// Byte length of the pinned detector config.
pub const DET_YML_LEN: usize = 903;

/// SHA-256 of [`DET_YML`].
pub const DET_YML_SHA256: &str = "98069072e1b6b37d727fd9d9f11725faa46d6ea0de012f2ed26caea011c37699";

/// Pinned detector-config download.
pub const DET_YML_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv5_mobile_det_onnx/resolve/main/inference.yml";

#[derive(Debug, PartialEq, Eq)]
pub enum ScreenModelError {
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

/// Where verified detector files are stored.
///
/// `W3COS_SCREEN_TEXT_DIR` wins. Otherwise `$XDG_DATA_HOME/w3cos/screen-text`
/// or `~/.local/share/w3cos/screen-text`.
pub fn default_screen_text_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("W3COS_SCREEN_TEXT_DIR") {
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
    base.join("w3cos").join("screen-text")
}

/// `true` only when the detector pair and the handwriting recognizer match.
pub fn screen_text_ready(det_dir: &Path, rec_dir: &Path) -> bool {
    file_matches(&det_dir.join(DET_FILE), DET_SHA256, DET_LEN as u64)
        && file_matches(&det_dir.join(DET_YML), DET_YML_SHA256, DET_YML_LEN as u64)
        && handwriting_model_ready(rec_dir)
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

fn map_ink(error: InkModelError) -> ScreenModelError {
    match error {
        InkModelError::ConsentRequired => ScreenModelError::ConsentRequired,
        InkModelError::ChecksumMismatch { expected, actual } => {
            ScreenModelError::ChecksumMismatch { expected, actual }
        }
        InkModelError::TooLarge => ScreenModelError::TooLarge,
        InkModelError::Http(message) => ScreenModelError::Http(message),
        InkModelError::Io(message) => ScreenModelError::Io(message),
    }
}

/// Download the detector, and the recognizer when it is not verified yet.
/// `consent == false` returns before any connection.
pub fn download_screen_text_model(
    det_dir: &Path,
    rec_dir: &Path,
    consent: bool,
) -> Result<(), ScreenModelError> {
    if !consent {
        return Err(ScreenModelError::ConsentRequired);
    }
    if !handwriting_model_ready(rec_dir) {
        handwriting_model::download_handwriting_model(rec_dir, true).map_err(map_ink)?;
    }
    download_pair(
        det_dir,
        true,
        DET_URL,
        DET_SHA256,
        DET_LEN,
        DET_FILE,
        DET_YML_URL,
        DET_YML_SHA256,
        DET_YML_LEN,
        DET_YML,
    )
    .map(|_| ())
    .map_err(map_ink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handwriting_model::{
        DICT_FILE, DICT_LEN, DICT_SHA256, INK_MODEL_FILE, INK_MODEL_LEN, INK_MODEL_SHA256,
    };
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn scratch(prefix: &str) -> PathBuf {
        let mut nonce = [0u8; 8];
        getrandom::getrandom(&mut nonce).unwrap();
        let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("{prefix}-{name}"));
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
    fn pinned_detector_identity_is_stable() {
        assert_eq!(DET_SHA256.len(), 64);
        assert_eq!(DET_YML_SHA256.len(), 64);
        assert!(DET_SHA256.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(DET_YML_SHA256.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(DET_URL.contains("PP-OCRv5_mobile_det_onnx"));
        assert!(DET_URL.ends_with(DET_FILE));
        assert!(DET_YML_URL.ends_with(DET_YML));
        assert_ne!(DET_SHA256, INK_MODEL_SHA256);
        assert_ne!(DET_LEN, INK_MODEL_LEN);
        assert_ne!(DET_YML_SHA256, DICT_SHA256);
        let _ = (INK_MODEL_FILE, DICT_FILE, DICT_LEN);
        const _: () = assert!(DET_LEN > 1_000_000);
        const _: () = assert!(DET_YML_LEN > 100);
    }

    #[test]
    fn missing_detector_is_not_ready() {
        let det = TempDir(scratch("w3cos-det"));
        let rec = TempDir(scratch("w3cos-rec"));
        assert!(!screen_text_ready(&det.0, &rec.0));
        fs::write(det.0.join(DET_FILE), b"not-the-detector").unwrap();
        fs::write(det.0.join(DET_YML), b"not-the-config").unwrap();
        assert!(!screen_text_ready(&det.0, &rec.0));
    }

    #[test]
    fn a_changed_detector_stops_matching() {
        let dir = TempDir(scratch("w3cos-det-flip"));
        let payload = b"hello-det-bytes!!";
        let hash = sha256_hex(payload);
        let path = dir.0.join("stand-in.bin");
        fs::write(&path, payload).unwrap();
        assert!(file_matches(&path, &hash, payload.len() as u64));
        let mut flipped = payload.to_vec();
        flipped[0] ^= 0xff;
        fs::write(&path, &flipped).unwrap();
        assert!(!file_matches(&path, &hash, payload.len() as u64));
    }

    #[test]
    fn refused_consent_opens_no_socket() {
        let det = TempDir(scratch("w3cos-det-consent"));
        let rec = TempDir(scratch("w3cos-rec-consent"));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let err = download_screen_text_model(&det.0, &rec.0, false).unwrap_err();
        assert_eq!(err, ScreenModelError::ConsentRequired);
        assert!(listener.accept().is_err(), "consent false still connected");
        assert!(!det.0.join(DET_FILE).exists());
        assert!(!det.0.join(DET_YML).exists());
        assert!(!rec.0.join(INK_MODEL_FILE).exists());
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
    fn a_bad_detector_body_is_dropped_and_leaves_no_file() {
        let det = TempDir(scratch("w3cos-det-bad"));
        let yml_port = serve(b"nope-yml");
        let model_port = serve(b"nope-model");
        let err = download_pair(
            &det.0,
            true,
            &format!("http://127.0.0.1:{model_port}/{DET_FILE}"),
            DET_SHA256,
            64,
            DET_FILE,
            &format!("http://127.0.0.1:{yml_port}/{DET_YML}"),
            DET_YML_SHA256,
            64,
            DET_YML,
        )
        .unwrap_err();
        assert!(matches!(
            map_ink(err),
            ScreenModelError::ChecksumMismatch { .. }
        ));
        assert!(!det.0.join(DET_FILE).exists());
        assert!(!det.0.join(DET_YML).exists());
        assert!(!screen_text_ready(&det.0, &det.0));
    }
}
