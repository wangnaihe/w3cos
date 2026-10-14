//! Encrypted-at-rest receipt file.
//!
//! Layout: magic `W3R1` || 12-byte nonce || ChaCha20-Poly1305 ciphertext.
//! The 32-byte key is created once beside the file (mode `0o600`) and reused
//! so a later process can decrypt. Callers write only when a receipt or the
//! unlock string changes — not while laying out, painting, or editing text.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};

const MAGIC: &[u8; 4] = b"W3R1";
const KEY_LEN: usize = 32;

pub struct ReceiptLedger {
    data_path: PathBuf,
    key: [u8; KEY_LEN],
}

impl ReceiptLedger {
    pub fn open_default() -> io::Result<Self> {
        let dir = receipt_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no home or W3COS_RECEIPT_DIR")
        })?;
        Self::open(&dir)
    }

    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        let key = load_or_create_key(&dir.join("receipts.key"))?;
        Ok(Self {
            data_path: dir.join("receipts.bin"),
            key,
        })
    }

    /// `Ok(None)` when no file has been written yet. A bad key or truncated
    /// file is an error so the caller can keep the previous bytes on disk.
    pub fn load(&self) -> io::Result<Option<Vec<u8>>> {
        if !self.data_path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&self.data_path)?;
        if bytes.len() < MAGIC.len() + ring::aead::NONCE_LEN + ring::aead::MAX_TAG_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "receipt file is too short",
            ));
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "receipt file magic mismatch",
            ));
        }
        let mut nonce_bytes = [0u8; ring::aead::NONCE_LEN];
        nonce_bytes.copy_from_slice(&bytes[MAGIC.len()..MAGIC.len() + ring::aead::NONCE_LEN]);
        let mut in_out = bytes[MAGIC.len() + ring::aead::NONCE_LEN..].to_vec();
        let key = open_key(&self.key)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let plain = key
            .open_in_place(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "receipt file did not open"))?;
        Ok(Some(plain.to_vec()))
    }

    pub fn store(&self, plaintext: &[u8]) -> io::Result<()> {
        let mut nonce_bytes = [0u8; ring::aead::NONCE_LEN];
        getrandom::fill(&mut nonce_bytes)
            .map_err(|err| io::Error::other(format!("receipt nonce entropy: {err}")))?;
        let key = open_key(&self.key)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut body = plaintext.to_vec();
        key.seal_in_place_append_tag(nonce, Aad::empty(), &mut body)
            .map_err(|_| io::Error::other("receipt seal failed"))?;
        let mut sealed = Vec::with_capacity(MAGIC.len() + nonce_bytes.len() + body.len());
        sealed.extend_from_slice(MAGIC);
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&body);

        if let Some(parent) = self.data_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.data_path.with_extension("tmp");
        {
            let mut file = create_private(&tmp)?;
            file.write_all(&sealed)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.data_path)?;
        Ok(())
    }
}

fn receipt_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("W3COS_RECEIPT_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".w3cos"))
}

fn open_key(key: &[u8; KEY_LEN]) -> io::Result<LessSafeKey> {
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, key)
        .map_err(|_| io::Error::other("receipt key was rejected"))?;
    Ok(LessSafeKey::new(unbound))
}

fn load_or_create_key(path: &Path) -> io::Result<[u8; KEY_LEN]> {
    if path.exists() {
        let bytes = fs::read(path)?;
        let key: [u8; KEY_LEN] = bytes.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "receipt key file is not 32 bytes",
            )
        })?;
        return Ok(key);
    }
    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key)
        .map_err(|err| io::Error::other(format!("receipt key entropy: {err}")))?;
    {
        let mut file = create_private(path)?;
        file.write_all(&key)?;
        file.sync_all()?;
    }
    Ok(key)
}

fn create_private(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
