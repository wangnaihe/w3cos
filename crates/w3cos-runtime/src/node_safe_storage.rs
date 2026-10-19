//! Electron `safeStorage` on a local keyring (M3 item 21).
//!
//! Code-OSS keeps its secrets (auth tokens, settings-sync keys) as
//! `safeStorage.encryptString()` blobs. Instead of the OS keychain /
//! libsecret / kwallet, w3cos seals them with AES-256-GCM under a per-user
//! key in its own local keyring directory, so encryption is always
//! available, offline, and never leaves the machine. The Electron-shaped
//! API and its errors are glue in `w3cos-node/js/electron.js`; these are the
//! host functions on `__w3cos_node_host`.
//!
//! Blob layout: `b"w3c1"` | 12-byte random nonce | ciphertext | 16-byte tag.
//! The key is 32 random bytes in `<keyring>/safe-storage.key` (mode 0600,
//! directory 0700), created on first use. `<keyring>` is
//! `$W3COS_KEYRING_DIR`, else `$XDG_DATA_HOME/w3cos/keyring`
//! (`~/.local/share/w3cos/keyring`; `%APPDATA%\w3cos\keyring` on Windows).

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use w3cos_core::Value;

/// Version prefix of a sealed blob (Electron's Linux blobs start `v10`/`v11`).
const PREFIX: &[u8] = b"w3c1";
const NONCE_LEN: usize = 12;
const KEY_FILE: &str = "safe-storage.key";
/// What `safeStorage.getSelectedStorageBackend()` reports.
pub(crate) const BACKEND: &str = "w3cos_keyring";

pub(crate) fn install(host: &Value) {
    use w3cos_core::node_host::define;
    // safeStorageEncrypt(text) -> { blob: ArrayBuffer } | { error }
    define(host, "safeStorageEncrypt", |args| {
        let text = args.first().map(Value::to_js_string).unwrap_or_default();
        match seal(text.as_bytes()) {
            Ok(blob) => object(vec![("blob", w3cos_core::binary::array_buffer_value(blob))]),
            Err(error) => error_value("unavailable", &error),
        }
    });
    // safeStorageDecrypt(bytes) -> { text } | { error: { code } }
    define(host, "safeStorageDecrypt", |args| {
        let bytes = args
            .first()
            .and_then(w3cos_core::binary::bytes_of)
            .unwrap_or_default();
        match open(&bytes) {
            Ok(plain) => object(vec![(
                "text",
                Value::from(String::from_utf8_lossy(&plain).into_owned()),
            )]),
            Err(Failure::NotEncrypted) => error_value(
                "notEncrypted",
                "Ciphertext does not appear to be encrypted.",
            ),
            Err(Failure::Corrupt) => error_value("failed", "Decryption failed."),
            Err(Failure::Key(error)) => error_value("unavailable", &error),
        }
    });
    define(host, "safeStorageBackend", |_| Value::from(BACKEND));
}

#[derive(Debug, PartialEq)]
enum Failure {
    NotEncrypted,
    Corrupt,
    Key(String),
}

fn keyring_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("W3COS_KEYRING_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    };
    base.map(|base| base.join("w3cos").join("keyring"))
}

/// The keyring key, created (exclusively, so racing threads or processes
/// agree on one key) on first use.
fn key() -> Result<LessSafeKey, String> {
    let dir = keyring_dir().ok_or_else(|| "no keyring directory".to_string())?;
    let path = dir.join(KEY_FILE);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_dir_private(&dir).map_err(|e| e.to_string())?;
            let mut fresh = [0_u8; 32];
            getrandom::fill(&mut fresh).map_err(|e| format!("{e:?}"))?;
            match create_file_private(&path) {
                Ok(mut file) => {
                    file.write_all(&fresh).map_err(|e| e.to_string())?;
                    file.sync_all().map_err(|e| e.to_string())?;
                    fresh.to_vec()
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::fs::read(&path).map_err(|e| e.to_string())?
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(error) => return Err(error.to_string()),
    };
    let unbound = UnboundKey::new(&AES_256_GCM, &raw)
        .map_err(|_| format!("{} is not a 256-bit key", path.display()))?;
    Ok(LessSafeKey::new(unbound))
}

#[cfg(unix)]
fn create_dir_private(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_dir_private(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn create_file_private(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_file_private(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    let key = key()?;
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| format!("{e:?}"))?;
    let mut sealed = plain.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(PREFIX),
        &mut sealed,
    )
    .map_err(|_| "encryption failed".to_string())?;
    let mut blob = Vec::with_capacity(PREFIX.len() + NONCE_LEN + sealed.len());
    blob.extend_from_slice(PREFIX);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

fn open(blob: &[u8]) -> Result<Vec<u8>, Failure> {
    let Some(rest) = blob.strip_prefix(PREFIX) else {
        return Err(Failure::NotEncrypted);
    };
    let Some((nonce, sealed)) = rest.split_at_checked(NONCE_LEN) else {
        return Err(Failure::Corrupt);
    };
    let key = key().map_err(Failure::Key)?;
    let mut buffer = sealed.to_vec();
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| Failure::Corrupt)?;
    let len = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(PREFIX),
            &mut buffer,
        )
        .map_err(|_| Failure::Corrupt)?
        .len();
    buffer.truncate(len);
    Ok(buffer)
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    let value = Value::object(HashMap::new());
    for (key, item) in entries {
        value.set_property(key, item);
    }
    value
}

fn error_value(code: &str, message: &str) -> Value {
    object(vec![(
        "error",
        object(vec![
            ("code", Value::from(code)),
            ("message", Value::from(message)),
        ]),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens_under_the_keyring_key() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module are the only readers of this variable.
        unsafe { std::env::set_var("W3COS_KEYRING_DIR", dir.path()) };
        let blob = seal("secret ✓".as_bytes()).unwrap();
        assert!(blob.starts_with(PREFIX));
        assert!(!blob.windows(6).any(|w| w == b"secret"));
        assert_eq!(open(&blob).unwrap(), "secret ✓".as_bytes());
        // Fresh nonce per seal.
        assert_ne!(seal(b"x").unwrap(), seal(b"x").unwrap());
        assert_eq!(open(b"garbage").unwrap_err(), Failure::NotEncrypted);
        let mut tampered = blob.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(open(&tampered).unwrap_err(), Failure::Corrupt);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        unsafe { std::env::remove_var("W3COS_KEYRING_DIR") };
    }
}
