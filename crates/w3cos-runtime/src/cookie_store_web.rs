//! Session-backed Cookie Store API compatibility layer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use w3cos_core::Value;

thread_local! {
    static COOKIES: RefCell<Vec<CookieEntry>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_URL: RefCell<Option<String>> = const { RefCell::new(None) };
    static COOKIE_STORE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static COOKIE_STORE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static COOKIE_CHANGE_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERSISTENCE_CONFIG: RefCell<Option<PersistenceConfig>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    static PERSISTENCE_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

const PERSISTENT_COOKIE_SCHEMA_VERSION: u32 = 1;
const MAX_COOKIE_NAME_VALUE_BYTES: usize = 4_096;
const MAX_COOKIES_PER_SITE: usize = 180;
const MAX_COOKIES_TOTAL: usize = 3_000;
const MAX_PERSISTENT_COOKIE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const PROTECTED_COOKIE_MAGIC: &[u8; 8] = b"W3COOKIE";
const PROTECTED_COOKIE_FORMAT_VERSION: u32 = 1;
const MAX_PROFILE_ID_BYTES: usize = 64;

/// Encrypts persistent Cookie Store bytes using an embedder-owned platform
/// credential. Implementations are expected to use facilities such as
/// Keychain, Android Keystore, DPAPI, or a desktop secret service and must bind
/// the ciphertext to `profile_id`.
pub trait CookiePersistenceProtector: Send + Sync {
    fn seal(&self, profile_id: &str, plaintext: &[u8]) -> std::io::Result<Vec<u8>>;
    fn open(&self, profile_id: &str, ciphertext: &[u8]) -> std::io::Result<Vec<u8>>;
}

/// Apple Keychain-backed protector for macOS and iOS embedders.
///
/// A random AES-256-GCM key is stored as a generic-password item. Cookie bytes
/// remain in the profile file only as authenticated ciphertext, with the
/// profile identifier used as associated data.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub struct AppleKeychainCookieProtector {
    service: String,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl AppleKeychainCookieProtector {
    pub fn new(service: impl Into<String>) -> std::io::Result<Self> {
        let service = service.into();
        if service.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Apple Keychain cookie service must not be empty",
            ));
        }
        Ok(Self { service })
    }

    fn key_for_profile(&self, profile_id: &str) -> std::io::Result<[u8; 32]> {
        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;
        match security_framework::passwords::get_generic_password(&self.service, profile_id) {
            Ok(key) => key.try_into().map_err(|_| {
                std::io::Error::other("Apple Keychain cookie key has an invalid length")
            }),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
                let mut generated = [0_u8; 32];
                getrandom::fill(&mut generated).map_err(|error| {
                    std::io::Error::other(format!("failed to generate Apple cookie key: {error:?}"))
                })?;
                security_framework::passwords::set_generic_password(
                    &self.service,
                    profile_id,
                    &generated,
                )
                .map_err(|error| std::io::Error::other(error.to_string()))?;
                let stored =
                    security_framework::passwords::get_generic_password(&self.service, profile_id)
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                stored.try_into().map_err(|_| {
                    std::io::Error::other("Apple Keychain cookie key has an invalid length")
                })
            }
            Err(error) => Err(std::io::Error::other(error.to_string())),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl CookiePersistenceProtector for AppleKeychainCookieProtector {
    fn seal(&self, profile_id: &str, plaintext: &[u8]) -> std::io::Result<Vec<u8>> {
        let key = self.key_for_profile(profile_id)?;
        seal_apple_cookie_bytes(&key, profile_id, plaintext)
    }

    fn open(&self, profile_id: &str, ciphertext: &[u8]) -> std::io::Result<Vec<u8>> {
        let key = self.key_for_profile(profile_id)?;
        open_apple_cookie_bytes(&key, profile_id, ciphertext)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn seal_apple_cookie_bytes(
    raw_key: &[u8; 32],
    profile_id: &str,
    plaintext: &[u8],
) -> std::io::Result<Vec<u8>> {
    use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

    let key =
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, raw_key).map_err(|_| {
            std::io::Error::other("failed to initialize Apple cookie encryption key")
        })?);
    let mut nonce = [0_u8; 12];
    getrandom::fill(&mut nonce).map_err(|error| {
        std::io::Error::other(format!("failed to generate Apple cookie nonce: {error:?}"))
    })?;
    let mut ciphertext = plaintext.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(profile_id.as_bytes()),
        &mut ciphertext,
    )
    .map_err(|_| std::io::Error::other("failed to encrypt Apple cookie data"))?;
    let mut protected = Vec::with_capacity(nonce.len() + ciphertext.len());
    protected.extend_from_slice(&nonce);
    protected.extend_from_slice(&ciphertext);
    Ok(protected)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn open_apple_cookie_bytes(
    raw_key: &[u8; 32],
    profile_id: &str,
    ciphertext: &[u8],
) -> std::io::Result<Vec<u8>> {
    use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

    let Some((nonce, ciphertext)) = ciphertext.split_at_checked(12) else {
        return Err(std::io::Error::other(
            "Apple Keychain cookie ciphertext is truncated",
        ));
    };
    let key =
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, raw_key).map_err(|_| {
            std::io::Error::other("failed to initialize Apple cookie decryption key")
        })?);
    let mut plaintext = ciphertext.to_vec();
    let plaintext_len = key
        .open_in_place(
            Nonce::assume_unique_for_key(
                nonce
                    .try_into()
                    .expect("Apple cookie nonce has fixed width"),
            ),
            Aad::from(profile_id.as_bytes()),
            &mut plaintext,
        )
        .map_err(|_| std::io::Error::other("Apple cookie ciphertext authentication failed"))?
        .len();
    plaintext.truncate(plaintext_len);
    Ok(plaintext)
}

#[derive(Clone)]
struct PersistenceConfig {
    path: PathBuf,
    profile_id: Option<String>,
    protector: Option<Arc<dyn CookiePersistenceProtector>>,
}

#[derive(Clone, Debug)]
struct CookieEntry {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
    http_only: bool,
    expires_at: Option<SystemTime>,
    same_site: CookieSameSite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum CookieSameSite {
    Strict,
    Lax,
    None,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistentCookieFile {
    schema_version: u32,
    cookies: Vec<PersistentCookie>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistentCookie {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
    http_only: bool,
    expires_unix_seconds: u64,
    same_site: CookieSameSite,
}

#[derive(Clone, Debug)]
pub(crate) struct CookieSnapshot {
    cookies: Vec<CookieEntry>,
}

struct RequestUrl {
    scheme: String,
    host: String,
    path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CookieRequestKind {
    Subresource,
    TopLevelNavigation { safe_method: bool },
}

#[derive(Clone, Debug)]
struct CookieRequestContext {
    site_for_cookies: Option<String>,
    kind: CookieRequestKind,
}

impl CookieRequestContext {
    fn same_site(url: &str) -> Self {
        Self {
            site_for_cookies: schemeful_site(url),
            kind: CookieRequestKind::Subresource,
        }
    }

    fn subresource(site_for_cookies_url: &str) -> Self {
        Self {
            site_for_cookies: schemeful_site(site_for_cookies_url),
            kind: CookieRequestKind::Subresource,
        }
    }

    fn top_level_navigation(site_for_cookies_url: &str, safe_method: bool) -> Self {
        Self {
            site_for_cookies: schemeful_site(site_for_cookies_url),
            kind: CookieRequestKind::TopLevelNavigation { safe_method },
        }
    }
}

fn current_url() -> String {
    ACTIVE_URL
        .with(|url| url.borrow().clone())
        .unwrap_or_else(crate::history::get_origin)
}

pub(crate) fn active_document_url() -> String {
    current_url()
}

pub(crate) fn active_document_url_if_set() -> Option<String> {
    ACTIVE_URL.with(|url| url.borrow().clone())
}

/// Binds persistent cookies to an embedder-owned data directory and loads the
/// current jar. Session cookies are intentionally not serialized.
pub fn set_persistence_dir(directory: PathBuf) -> std::io::Result<()> {
    let config = PersistenceConfig {
        path: directory.join("cookies.json"),
        profile_id: None,
        protector: None,
    };
    let cookies = load_persistent_cookies(&config)?;
    COOKIES.with(|slot| *slot.borrow_mut() = cookies);
    PERSISTENCE_CONFIG.with(|slot| *slot.borrow_mut() = Some(config));
    flush_persistent_cookies()
}

/// Binds the Cookie Store to one explicitly named, encrypted profile.
///
/// The profile identifier is encoded into a path-safe directory and into the
/// authenticated protector context. Plaintext or another profile's envelope
/// is rejected rather than silently downgraded.
pub fn set_encrypted_persistence_profile(
    root_directory: PathBuf,
    profile_id: &str,
    protector: Arc<dyn CookiePersistenceProtector>,
) -> std::io::Result<()> {
    validate_profile_id(profile_id)?;
    let config = PersistenceConfig {
        path: encrypted_profile_cookie_path(&root_directory, profile_id),
        profile_id: Some(profile_id.to_string()),
        protector: Some(protector),
    };
    let cookies = load_persistent_cookies(&config)?;
    COOKIES.with(|slot| *slot.borrow_mut() = cookies);
    PERSISTENCE_CONFIG.with(|slot| *slot.borrow_mut() = Some(config));
    flush_persistent_cookies()
}

fn validate_profile_id(profile_id: &str) -> std::io::Result<()> {
    if profile_id.is_empty() || profile_id.len() > MAX_PROFILE_ID_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "cookie profile identifier must contain 1..={MAX_PROFILE_ID_BYTES} UTF-8 bytes"
            ),
        ));
    }
    Ok(())
}

fn encrypted_profile_cookie_path(root_directory: &Path, profile_id: &str) -> PathBuf {
    let encoded_profile = profile_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    root_directory
        .join("cookie-profiles")
        .join(encoded_profile)
        .join("cookies.bin")
}

fn load_persistent_cookies(config: &PersistenceConfig) -> std::io::Result<Vec<CookieEntry>> {
    let encoded = match std::fs::read(&config.path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if encoded.len() as u64 > MAX_PERSISTENT_COOKIE_FILE_BYTES {
        return Err(std::io::Error::other(
            "persistent cookie file exceeds the storage limit",
        ));
    }
    let plaintext = match (&config.profile_id, &config.protector) {
        (Some(profile_id), Some(protector)) => {
            let ciphertext = decode_protected_cookie_envelope(&encoded, profile_id)?;
            protector.open(profile_id, ciphertext)?
        }
        (None, None) => encoded,
        _ => {
            return Err(std::io::Error::other(
                "persistent cookie encryption configuration is incomplete",
            ));
        }
    };
    if plaintext.len() as u64 > MAX_PERSISTENT_COOKIE_FILE_BYTES {
        return Err(std::io::Error::other(
            "decrypted persistent cookie file exceeds the storage limit",
        ));
    }
    let file: PersistentCookieFile =
        serde_json::from_slice(&plaintext).map_err(std::io::Error::other)?;
    if file.schema_version != PERSISTENT_COOKIE_SCHEMA_VERSION {
        return Err(std::io::Error::other(
            "unsupported persistent cookie schema version",
        ));
    }
    let now = SystemTime::now();
    let mut cookies = file
        .cookies
        .into_iter()
        .filter_map(|cookie| {
            let expires_at = SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(cookie.expires_unix_seconds))?;
            if expires_at <= now
                || cookie.name.is_empty()
                || cookie.domain.is_empty()
                || !cookie.path.starts_with('/')
                || (!cookie.host_only && is_public_suffix(&cookie.domain))
                || (cookie.same_site == CookieSameSite::None && !cookie.secure)
            {
                return None;
            }
            Some(CookieEntry {
                name: cookie.name,
                value: cookie.value,
                domain: cookie.domain,
                host_only: cookie.host_only,
                path: cookie.path,
                secure: cookie.secure,
                http_only: cookie.http_only,
                expires_at: Some(expires_at),
                same_site: cookie.same_site,
            })
        })
        .collect::<Vec<_>>();
    enforce_cookie_quotas(&mut cookies);
    Ok(cookies)
}

fn flush_persistent_cookies() -> std::io::Result<()> {
    let Some(config) = PERSISTENCE_CONFIG.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let now = SystemTime::now();
    let cookies = COOKIES.with(|cookies| {
        cookies
            .borrow()
            .iter()
            .filter_map(|cookie| {
                let expires_at = cookie.expires_at?;
                let expires_unix_seconds = expires_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                (expires_at > now).then(|| PersistentCookie {
                    name: cookie.name.clone(),
                    value: cookie.value.clone(),
                    domain: cookie.domain.clone(),
                    host_only: cookie.host_only,
                    path: cookie.path.clone(),
                    secure: cookie.secure,
                    http_only: cookie.http_only,
                    expires_unix_seconds,
                    same_site: cookie.same_site,
                })
            })
            .collect()
    });
    let file = PersistentCookieFile {
        schema_version: PERSISTENT_COOKIE_SCHEMA_VERSION,
        cookies,
    };
    let plaintext = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
    let encoded = match (&config.profile_id, &config.protector) {
        (Some(profile_id), Some(protector)) => {
            let ciphertext = protector.seal(profile_id, &plaintext)?;
            encode_protected_cookie_envelope(profile_id, &ciphertext)?
        }
        (None, None) => plaintext,
        _ => {
            return Err(std::io::Error::other(
                "persistent cookie encryption configuration is incomplete",
            ));
        }
    };
    if encoded.len() as u64 > MAX_PERSISTENT_COOKIE_FILE_BYTES {
        return Err(std::io::Error::other(
            "persistent cookie file exceeds the storage limit",
        ));
    }
    let Some(directory) = config.path.parent() else {
        return Err(std::io::Error::other(
            "persistent cookie path has no parent directory",
        ));
    };
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&encoded)?;
    temporary.flush()?;
    temporary
        .persist(config.path)
        .map_err(|error| error.error)?;
    Ok(())
}

fn encode_protected_cookie_envelope(
    profile_id: &str,
    ciphertext: &[u8],
) -> std::io::Result<Vec<u8>> {
    let profile_len = u16::try_from(profile_id.len())
        .map_err(|_| std::io::Error::other("cookie profile identifier is too long"))?;
    let ciphertext_len = u64::try_from(ciphertext.len())
        .map_err(|_| std::io::Error::other("protected cookie payload is too large"))?;
    let mut encoded =
        Vec::with_capacity(8 + 4 + 2 + 8 + usize::from(profile_len) + ciphertext.len());
    encoded.extend_from_slice(PROTECTED_COOKIE_MAGIC);
    encoded.extend_from_slice(&PROTECTED_COOKIE_FORMAT_VERSION.to_le_bytes());
    encoded.extend_from_slice(&profile_len.to_le_bytes());
    encoded.extend_from_slice(&ciphertext_len.to_le_bytes());
    encoded.extend_from_slice(profile_id.as_bytes());
    encoded.extend_from_slice(ciphertext);
    Ok(encoded)
}

fn decode_protected_cookie_envelope<'a>(
    encoded: &'a [u8],
    expected_profile_id: &str,
) -> std::io::Result<&'a [u8]> {
    const HEADER_LEN: usize = 8 + 4 + 2 + 8;
    if encoded.len() < HEADER_LEN || &encoded[..8] != PROTECTED_COOKIE_MAGIC {
        return Err(std::io::Error::other(
            "encrypted cookie profile has no valid protected envelope",
        ));
    }
    let version = u32::from_le_bytes(
        encoded[8..12]
            .try_into()
            .expect("protected cookie version has fixed width"),
    );
    if version != PROTECTED_COOKIE_FORMAT_VERSION {
        return Err(std::io::Error::other(
            "unsupported protected cookie envelope version",
        ));
    }
    let profile_len = usize::from(u16::from_le_bytes(
        encoded[12..14]
            .try_into()
            .expect("protected cookie profile length has fixed width"),
    ));
    let ciphertext_len = usize::try_from(u64::from_le_bytes(
        encoded[14..22]
            .try_into()
            .expect("protected cookie payload length has fixed width"),
    ))
    .map_err(|_| std::io::Error::other("protected cookie payload is too large"))?;
    let profile_end = HEADER_LEN
        .checked_add(profile_len)
        .ok_or_else(|| std::io::Error::other("protected cookie envelope length overflow"))?;
    let payload_end = profile_end
        .checked_add(ciphertext_len)
        .ok_or_else(|| std::io::Error::other("protected cookie envelope length overflow"))?;
    if payload_end != encoded.len() {
        return Err(std::io::Error::other(
            "protected cookie envelope has an invalid length",
        ));
    }
    let profile =
        std::str::from_utf8(&encoded[HEADER_LEN..profile_end]).map_err(std::io::Error::other)?;
    if profile != expected_profile_id {
        return Err(std::io::Error::other(
            "protected cookie envelope belongs to another profile",
        ));
    }
    Ok(&encoded[profile_end..payload_end])
}

fn flush_persistent_cookies_or_warn() {
    if let Err(error) = flush_persistent_cookies() {
        PERSISTENCE_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!("[w3cos] warning: failed to persist Cookie Store: {error}");
            }
        });
    }
}

pub fn set_active_origin(origin: &str) {
    set_active_url(origin);
}

pub fn set_active_url(url: &str) {
    ACTIVE_URL.with(|active| *active.borrow_mut() = Some(url.to_string()));
}

fn parse_request_url(url: &str) -> Option<RequestUrl> {
    let (scheme, remainder) = url.split_once("://")?;
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let tail = &remainder[authority_end..];
    let path = if tail.starts_with('/') {
        tail.split(['?', '#']).next().unwrap_or("/").to_string()
    } else {
        "/".to_string()
    };
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if authority.starts_with('[') {
        authority
            .split_once(']')
            .map(|(host, _)| format!("{host}]"))?
    } else {
        authority
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|character| character.is_ascii_digit()))
            .map_or(authority, |(host, _)| host)
            .to_string()
    };
    Some(RequestUrl {
        scheme: scheme.to_ascii_lowercase(),
        host: host.trim_end_matches('.').to_ascii_lowercase(),
        path,
    })
}

fn is_ip_literal(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok()
}

fn is_public_suffix(host: &str) -> bool {
    !is_ip_literal(host) && psl::suffix_str(host).is_some_and(|suffix| suffix == host)
}

fn schemeful_site(url: &str) -> Option<String> {
    let request = parse_request_url(url)?;
    schemeful_site_from_request(&request)
}

fn domain_matches(host: &str, cookie: &CookieEntry) -> bool {
    if cookie.host_only {
        host == cookie.domain
    } else {
        host == cookie.domain
            || host
                .strip_suffix(&cookie.domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    }
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || request_path
            .strip_prefix(cookie_path)
            .is_some_and(|suffix| cookie_path.ends_with('/') || suffix.starts_with('/'))
}

fn is_expired(cookie: &CookieEntry, now: SystemTime) -> bool {
    cookie
        .expires_at
        .is_some_and(|expires_at| expires_at <= now)
}

fn visible_cookies_for_request(
    url: &str,
    include_http_only: bool,
    context: &CookieRequestContext,
) -> Vec<CookieEntry> {
    let Some(request) = parse_request_url(url) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let (matches, removed_expired) = COOKIES.with(|cookies| {
        let mut cookies = cookies.borrow_mut();
        let previous_len = cookies.len();
        cookies.retain(|cookie| !is_expired(cookie, now));
        (
            matching_cookies(&cookies, &request, include_http_only, now, context),
            cookies.len() != previous_len,
        )
    });
    if removed_expired {
        flush_persistent_cookies_or_warn();
    }
    matches
}

fn visible_cookies_for_url(url: &str, include_http_only: bool) -> Vec<CookieEntry> {
    visible_cookies_for_request(
        url,
        include_http_only,
        &CookieRequestContext::same_site(url),
    )
}

fn matching_cookies(
    cookies: &[CookieEntry],
    request: &RequestUrl,
    include_http_only: bool,
    now: SystemTime,
    context: &CookieRequestContext,
) -> Vec<CookieEntry> {
    let request_site = schemeful_site_from_request(request);
    let same_site = request_site
        .as_ref()
        .zip(context.site_for_cookies.as_ref())
        .is_some_and(|(request, initiator)| request == initiator);
    let mut matches = cookies
        .iter()
        .filter(|cookie| {
            !is_expired(cookie, now)
                && domain_matches(&request.host, cookie)
                && path_matches(&request.path, &cookie.path)
                && (!cookie.secure || request.scheme == "https")
                && (include_http_only || !cookie.http_only)
                && match cookie.same_site {
                    CookieSameSite::None => true,
                    CookieSameSite::Strict => same_site,
                    CookieSameSite::Lax => {
                        same_site
                            || matches!(
                                context.kind,
                                CookieRequestKind::TopLevelNavigation { safe_method: true }
                            )
                    }
                }
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| right.path.len().cmp(&left.path.len()));
    matches
}

fn schemeful_site_from_request(request: &RequestUrl) -> Option<String> {
    Some(format!(
        "{}://{}",
        request.scheme,
        registrable_domain_from_request(request)
    ))
}

fn registrable_domain_from_request(request: &RequestUrl) -> &str {
    if is_ip_literal(&request.host) {
        request.host.as_str()
    } else {
        psl::domain_str(&request.host).unwrap_or(&request.host)
    }
}

fn cookie_site(cookie: &CookieEntry) -> &str {
    if is_ip_literal(&cookie.domain) {
        cookie.domain.as_str()
    } else {
        psl::domain_str(&cookie.domain).unwrap_or(&cookie.domain)
    }
}

fn enforce_cookie_quotas(cookies: &mut Vec<CookieEntry>) -> Vec<CookieEntry> {
    let mut evicted = Vec::new();
    while let Some(index) = cookies.iter().enumerate().find_map(|(index, cookie)| {
        let site = cookie_site(cookie);
        (cookies
            .iter()
            .filter(|candidate| cookie_site(candidate) == site)
            .count()
            > MAX_COOKIES_PER_SITE)
            .then_some(index)
    }) {
        evicted.push(cookies.remove(index));
    }
    while cookies.len() > MAX_COOKIES_TOTAL {
        evicted.push(cookies.remove(0));
    }
    evicted
}

pub(crate) fn urls_are_corp_same_site(initiator_url: &str, response_url: &str) -> bool {
    let Some(initiator) = parse_request_url(initiator_url) else {
        return false;
    };
    let Some(response) = parse_request_url(response_url) else {
        return false;
    };
    registrable_domain_from_request(&initiator) == registrable_domain_from_request(&response)
        && (initiator.scheme == "https" || response.scheme != "https")
}

pub(crate) fn snapshot() -> CookieSnapshot {
    let now = SystemTime::now();
    let (snapshot, removed_expired) = COOKIES.with(|cookies| {
        let mut cookies = cookies.borrow_mut();
        let previous_len = cookies.len();
        cookies.retain(|cookie| !is_expired(cookie, now));
        (
            CookieSnapshot {
                cookies: cookies.clone(),
            },
            cookies.len() != previous_len,
        )
    });
    if removed_expired {
        flush_persistent_cookies_or_warn();
    }
    snapshot
}

impl CookieSnapshot {
    pub(crate) fn header_for_url(&self, url: &str) -> String {
        self.header_for_request(url, &CookieRequestContext::same_site(url))
    }

    pub(crate) fn header_for_subresource(&self, url: &str, site_for_cookies_url: &str) -> String {
        self.header_for_request(
            url,
            &CookieRequestContext::subresource(site_for_cookies_url),
        )
    }

    pub(crate) fn header_for_top_level_navigation(
        &self,
        url: &str,
        site_for_cookies_url: &str,
        safe_method: bool,
    ) -> String {
        self.header_for_request(
            url,
            &CookieRequestContext::top_level_navigation(site_for_cookies_url, safe_method),
        )
    }

    fn header_for_request(&self, url: &str, context: &CookieRequestContext) -> String {
        let Some(request) = parse_request_url(url) else {
            return String::new();
        };
        matching_cookies(&self.cookies, &request, true, SystemTime::now(), context)
            .into_iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub(crate) fn apply_http_set_cookie(&mut self, url: &str, assignment: &str) {
        match parse_cookie_assignment(url, assignment, true) {
            Some(CookieMutation::Store(cookie)) => {
                self.cookies.retain(|candidate| {
                    candidate.name != cookie.name
                        || candidate.domain != cookie.domain
                        || candidate.path != cookie.path
                });
                self.cookies.push(cookie);
                enforce_cookie_quotas(&mut self.cookies);
            }
            Some(CookieMutation::Remove(cookie)) => {
                self.cookies.retain(|candidate| {
                    candidate.name != cookie.name
                        || candidate.domain != cookie.domain
                        || candidate.path != cookie.path
                });
            }
            None => {}
        }
    }
}

pub fn cookie_header_for_url(url: &str) -> String {
    visible_cookies_for_url(url, true)
        .into_iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn cookie_header_for_origin(origin: &str) -> String {
    cookie_header_for_url(origin)
}

fn cookie_value(cookie: &CookieEntry) -> Value {
    let expires = cookie
        .expires_at
        .and_then(|expires_at| expires_at.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(Value::Null, |duration| {
            Value::Number(duration.as_secs_f64() * 1_000.0)
        });
    let same_site = match cookie.same_site {
        CookieSameSite::Strict => "strict",
        CookieSameSite::Lax => "lax",
        CookieSameSite::None => "none",
    };
    Value::object(HashMap::from([
        ("name".into(), Value::string(&cookie.name)),
        ("value".into(), Value::string(&cookie.value)),
        ("domain".into(), Value::string(&cookie.domain)),
        ("path".into(), Value::string(&cookie.path)),
        ("expires".into(), expires),
        ("secure".into(), Value::Bool(cookie.secure)),
        ("sameSite".into(), Value::string(same_site)),
        ("partitioned".into(), Value::Bool(false)),
    ]))
}

fn change_event(changed: Vec<Value>, deleted: Vec<Value>) -> Value {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string("change")],
    );
    event.set_property("changed", Value::array(changed));
    event.set_property("deleted", Value::array(deleted));
    w3cos_core::class::set_prototype_of(
        &event,
        &cookie_change_event_class().get_property("prototype"),
    );
    event
}

fn dispatch_change(changed: Vec<Value>, deleted: Vec<Value>) {
    let store = cookie_store_value();
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        store.call_method(
            "dispatchEvent",
            vec![change_event(changed.clone(), deleted.clone())],
        );
        Value::Undefined
    }));
}

pub fn document_cookie() -> String {
    visible_cookies_for_url(&current_url(), false)
        .into_iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn set_document_cookie(assignment: &str) {
    set_cookie_assignment_for_url(&current_url(), assignment, false);
}

pub fn set_cookie_assignment_for_origin(origin: &str, assignment: &str) {
    set_cookie_assignment_for_url(origin, assignment, true);
}

enum CookieMutation {
    Store(CookieEntry),
    Remove(CookieEntry),
}

fn parse_cookie_assignment(url: &str, assignment: &str, from_http: bool) -> Option<CookieMutation> {
    let Some(request) = parse_request_url(url) else {
        return None;
    };
    let mut parts = assignment.split(';');
    let pair = parts.next().unwrap_or_default();
    let Some((name, value)) = pair.split_once('=') else {
        return None;
    };
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || name.len().saturating_add(value.len()) > MAX_COOKIE_NAME_VALUE_BYTES {
        return None;
    }
    let mut domain = request.host.clone();
    let mut host_only = true;
    let mut path = request
        .path
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
        .to_string();
    let mut secure = false;
    let mut http_only = false;
    let mut max_age = None;
    let mut expires = None;
    let mut same_site = CookieSameSite::Lax;
    for attribute in parts {
        let (attribute_name, attribute_value) = attribute
            .trim()
            .split_once('=')
            .map_or((attribute.trim(), ""), |(name, value)| {
                (name.trim(), value.trim())
            });
        match attribute_name.to_ascii_lowercase().as_str() {
            "domain" if !attribute_value.is_empty() => {
                let candidate = attribute_value
                    .trim_start_matches('.')
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if request.host != candidate
                    && !request
                        .host
                        .strip_suffix(&candidate)
                        .is_some_and(|prefix| prefix.ends_with('.'))
                {
                    return None;
                }
                if is_public_suffix(&candidate) {
                    if request.host != candidate {
                        return None;
                    }
                } else {
                    domain = candidate;
                    host_only = false;
                }
            }
            "path" if attribute_value.starts_with('/') => path = attribute_value.to_string(),
            "secure" => secure = true,
            "httponly" if from_http => http_only = true,
            "max-age" => max_age = attribute_value.parse::<i64>().ok(),
            "expires" => expires = parse_cookie_date(attribute_value),
            "samesite" => {
                same_site = match attribute_value.to_ascii_lowercase().as_str() {
                    "strict" => CookieSameSite::Strict,
                    "lax" => CookieSameSite::Lax,
                    "none" => CookieSameSite::None,
                    _ => same_site,
                };
            }
            _ => {}
        }
    }
    if secure && request.scheme != "https" {
        return None;
    }
    if same_site == CookieSameSite::None && !secure {
        return None;
    }
    let expires_at = match max_age {
        Some(seconds) if seconds > 0 => {
            SystemTime::now().checked_add(Duration::from_secs(seconds as u64))
        }
        Some(_) => None,
        None => expires,
    };
    let cookie = CookieEntry {
        name: name.to_string(),
        value: value.to_string(),
        domain,
        host_only,
        path,
        secure,
        http_only,
        expires_at,
        same_site,
    };
    Some(
        if max_age.is_some_and(|seconds| seconds <= 0)
            || expires_at.is_some_and(|expires_at| expires_at <= SystemTime::now())
        {
            CookieMutation::Remove(cookie)
        } else {
            CookieMutation::Store(cookie)
        },
    )
}

fn parse_cookie_date(value: &str) -> Option<SystemTime> {
    let tokens = value
        .split(|character: char| !character.is_ascii_alphanumeric() && character != ':')
        .filter(|token| !token.is_empty());
    let mut time = None;
    let mut day = None;
    let mut month = None;
    let mut year = None;
    for token in tokens {
        if time.is_none() {
            let mut fields = token.split(':');
            if let (Some(hour), Some(minute), Some(second), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
                && let (Ok(hour), Ok(minute), Ok(second)) = (
                    hour.parse::<u32>(),
                    minute.parse::<u32>(),
                    second.parse::<u32>(),
                )
            {
                time = Some((hour, minute, second));
                continue;
            }
        }
        if day.is_none()
            && token.len() <= 2
            && let Ok(candidate) = token.parse::<u32>()
            && (1..=31).contains(&candidate)
        {
            day = Some(candidate);
            continue;
        }
        if month.is_none() {
            month = match token.to_ascii_lowercase().as_str() {
                "jan" => Some(1),
                "feb" => Some(2),
                "mar" => Some(3),
                "apr" => Some(4),
                "may" => Some(5),
                "jun" => Some(6),
                "jul" => Some(7),
                "aug" => Some(8),
                "sep" => Some(9),
                "oct" => Some(10),
                "nov" => Some(11),
                "dec" => Some(12),
                _ => None,
            };
            if month.is_some() {
                continue;
            }
        }
        if year.is_none()
            && (2..=4).contains(&token.len())
            && let Ok(candidate) = token.parse::<i64>()
        {
            year = Some(match candidate {
                0..=69 => candidate + 2_000,
                70..=99 => candidate + 1_900,
                _ => candidate,
            });
        }
    }
    let (hour, minute, second) = time?;
    let (day, month, year) = (day?, month?, year?);
    if year < 1601
        || hour > 23
        || minute > 59
        || second > 59
        || !valid_cookie_date(year, month, day)
    {
        return None;
    }
    let seconds = i128::from(days_from_civil(year, month, day))
        .checked_mul(86_400)?
        .checked_add(i128::from(hour) * 3_600)?
        .checked_add(i128::from(minute) * 60)?
        .checked_add(i128::from(second))?;
    if seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(seconds).ok()?))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(u64::try_from(-seconds).ok()?))
    }
}

fn valid_cookie_date(year: i64, month: u32, day: u32) -> bool {
    let leap = year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn days_from_civil(mut year: i64, month: u32, day: u32) -> i64 {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub fn set_cookie_assignment_for_url(url: &str, assignment: &str, from_http: bool) {
    match parse_cookie_assignment(url, assignment, from_http) {
        Some(CookieMutation::Store(cookie)) => store_cookie(cookie),
        Some(CookieMutation::Remove(cookie)) => remove_cookie(&cookie),
        None => {}
    }
}

fn set_cookie(name: &str, value: &str) {
    set_document_cookie(&format!("{name}={value}; Path=/"));
}

fn store_cookie(cookie: CookieEntry) {
    let (previous, evicted) = COOKIES.with(|cookies| {
        let mut cookies = cookies.borrow_mut();
        let previous = cookies
            .iter()
            .find(|candidate| {
                candidate.name == cookie.name
                    && candidate.domain == cookie.domain
                    && candidate.path == cookie.path
            })
            .cloned();
        cookies.retain(|candidate| {
            candidate.name != cookie.name
                || candidate.domain != cookie.domain
                || candidate.path != cookie.path
        });
        cookies.push(cookie.clone());
        let evicted = enforce_cookie_quotas(&mut cookies);
        (previous, evicted)
    });
    flush_persistent_cookies_or_warn();
    if previous
        .as_ref()
        .is_none_or(|old| old.value != cookie.value)
    {
        dispatch_change(vec![cookie_value(&cookie)], vec![]);
    }
    if !evicted.is_empty() {
        dispatch_change(vec![], evicted.iter().map(cookie_value).collect::<Vec<_>>());
    }
}

fn remove_cookie(cookie: &CookieEntry) {
    let deleted = COOKIES.with(|cookies| {
        let mut cookies = cookies.borrow_mut();
        let deleted = cookies
            .iter()
            .find(|candidate| {
                candidate.name == cookie.name
                    && candidate.domain == cookie.domain
                    && candidate.path == cookie.path
            })
            .cloned();
        cookies.retain(|candidate| {
            candidate.name != cookie.name
                || candidate.domain != cookie.domain
                || candidate.path != cookie.path
        });
        deleted
    });
    if deleted.is_some() {
        flush_persistent_cookies_or_warn();
    }
    if let Some(cookie) = deleted {
        dispatch_change(vec![], vec![cookie_value(&cookie)]);
    }
}

fn delete_cookie(name: &str) {
    let Some(cookie) = visible_cookies_for_url(&current_url(), false)
        .into_iter()
        .find(|cookie| cookie.name == name)
    else {
        return;
    };
    remove_cookie(&cookie);
}

fn option_name(value: &Value) -> Option<String> {
    if matches!(value, Value::String(_)) {
        Some(value.to_js_string())
    } else {
        let name = value.get_property("name");
        (!name.is_undefined()).then(|| name.to_js_string())
    }
}

pub fn cookie_store_value() -> Value {
    COOKIE_STORE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: cookieStore uses the shared URL-matched cookie backend; \
                     persistence requires an embedder data directory; encrypted multi-profile \
                     persistence additionally requires a platform CookiePersistenceProtector, and \
                     service-worker delivery remains pending"
                );
            }
        });
        let store = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        store.set_property("onchange", Value::Null);
        store.set_property(
            "get",
            Value::function(|_, args| {
                let name = option_name(&args.first().cloned().unwrap_or(Value::Undefined));
                let found = name.and_then(|name| {
                    visible_cookies_for_url(&current_url(), false)
                        .into_iter()
                        .find(|cookie| cookie.name == name)
                        .map(|cookie| cookie_value(&cookie))
                });
                w3cos_core::promise::resolve(vec![found.unwrap_or(Value::Null)])
            }),
        );
        store.set_property(
            "getAll",
            Value::function(|_, args| {
                let selector = args.first().cloned().unwrap_or(Value::Undefined);
                let name = (!selector.is_undefined())
                    .then(|| option_name(&selector))
                    .flatten();
                let cookies = visible_cookies_for_url(&current_url(), false)
                    .into_iter()
                    .filter(|cookie| {
                        name.as_ref()
                            .is_none_or(|name| cookie.name.as_str() == name)
                    })
                    .map(|cookie| cookie_value(&cookie))
                    .collect();
                w3cos_core::promise::resolve(vec![Value::array(cookies)])
            }),
        );
        store.set_property(
            "set",
            Value::function(|_, args| {
                let first = args.first().cloned().unwrap_or(Value::Undefined);
                let (name, value) = if matches!(first, Value::String(_)) {
                    (
                        first.to_js_string(),
                        args.get(1)
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                    )
                } else {
                    (
                        first.get_property("name").to_js_string(),
                        first.get_property("value").to_js_string(),
                    )
                };
                if !name.is_empty() && name != "undefined" {
                    set_cookie(&name, &value);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        store.set_property(
            "delete",
            Value::function(|_, args| {
                if let Some(name) = option_name(&args.first().cloned().unwrap_or(Value::Undefined))
                {
                    delete_cookie(&name);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        w3cos_core::class::set_prototype_of(
            &store,
            &cookie_store_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(store.clone());
        store
    })
}

pub fn cookie_store_class() -> Value {
    COOKIE_STORE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| Value::Undefined);
        class.set_property("name", Value::string("CookieStore"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["delete", "get", "getAll", "set"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("onchange", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn cookie_change_event_class() -> Value {
    COOKIE_CHANGE_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("type", Value::string("change"));
            this.set_property("changed", init.get_property("changed"));
            this.set_property("deleted", init.get_property("deleted"));
            Value::Undefined
        });
        class.set_property("name", Value::string("CookieChangeEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("changed", Value::Undefined);
        prototype.set_property("deleted", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

/// Clears only document-scoped Cookie Store context. The browser jar survives
/// navigation and is rematched when the next document URL is attached.
pub fn reset_document_context() {
    ACTIVE_URL.with(|url| *url.borrow_mut() = None);
}

/// Clears in-memory state and disables persistence without deleting its file.
/// Intended for isolated runtime/test teardown; navigation uses
/// [`reset_document_context`] instead.
pub fn reset() {
    COOKIES.with(|cookies| cookies.borrow_mut().clear());
    PERSISTENCE_CONFIG.with(|config| *config.borrow_mut() = None);
    PERSISTENCE_WARNING_EMITTED.with(|warned| warned.set(false));
    reset_document_context();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    struct TestCookieProtector {
        key: u8,
    }

    impl TestCookieProtector {
        fn tag(&self, profile_id: &str, plaintext: &[u8]) -> u64 {
            profile_id.as_bytes().iter().chain(plaintext).fold(
                0xcbf2_9ce4_8422_2325_u64,
                |hash, byte| {
                    (hash ^ u64::from(*byte) ^ u64::from(self.key)).wrapping_mul(0x100_0000_01b3)
                },
            )
        }
    }

    impl CookiePersistenceProtector for TestCookieProtector {
        fn seal(&self, profile_id: &str, plaintext: &[u8]) -> std::io::Result<Vec<u8>> {
            let mut ciphertext = self.tag(profile_id, plaintext).to_le_bytes().to_vec();
            ciphertext.extend(plaintext.iter().map(|byte| byte ^ self.key));
            Ok(ciphertext)
        }

        fn open(&self, profile_id: &str, ciphertext: &[u8]) -> std::io::Result<Vec<u8>> {
            let Some((tag, ciphertext)) = ciphertext.split_at_checked(8) else {
                return Err(std::io::Error::other(
                    "protected cookie payload is truncated",
                ));
            };
            let plaintext = ciphertext
                .iter()
                .map(|byte| byte ^ self.key)
                .collect::<Vec<_>>();
            if tag != self.tag(profile_id, &plaintext).to_le_bytes() {
                return Err(std::io::Error::other(
                    "protected cookie authentication failed",
                ));
            }
            Ok(plaintext)
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn apple_cookie_ciphertext_is_authenticated_and_profile_bound() {
        let key = [0x5a; 32];
        let plaintext = br#"{"cookies":[{"name":"session"}]}"#;
        let mut protected = seal_apple_cookie_bytes(&key, "alpha", plaintext).unwrap();

        assert!(
            !protected
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );
        assert_eq!(
            open_apple_cookie_bytes(&key, "alpha", &protected).unwrap(),
            plaintext
        );
        assert!(open_apple_cookie_bytes(&key, "beta", &protected).is_err());

        let last = protected.len() - 1;
        protected[last] ^= 0x80;
        assert!(open_apple_cookie_bytes(&key, "alpha", &protected).is_err());
        assert!(open_apple_cookie_bytes(&key, "alpha", &[0; 11]).is_err());
    }

    #[test]
    fn cookie_store_and_document_cookie_share_state_and_emit_changes() {
        crate::jsdom::reset_bridge();
        let store = cookie_store_value();
        assert!(w3cos_core::class::instance_of(
            &store,
            &cookie_store_class()
        ));
        let changes = Rc::new(RefCell::new(Vec::<Value>::new()));
        let changes_for_listener = Rc::clone(&changes);
        store.call_method(
            "addEventListener",
            vec![
                Value::string("change"),
                Value::function(move |_, args| {
                    changes_for_listener.borrow_mut().push(args[0].clone());
                    Value::Undefined
                }),
            ],
        );

        set_document_cookie("theme=dark; Path=/");
        crate::jsdom::drain_microtasks();
        assert_eq!(document_cookie(), "theme=dark");
        assert_eq!(changes.borrow().len(), 1);
        assert!(w3cos_core::class::instance_of(
            &changes.borrow()[0],
            &cookie_change_event_class()
        ));
        assert_eq!(
            changes.borrow()[0]
                .get_property("changed")
                .get_property("length"),
            Value::Number(1.0)
        );

        let found = Rc::new(RefCell::new(Value::Undefined));
        let found_for_then = Rc::clone(&found);
        store
            .call_method("get", vec![Value::string("theme")])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *found_for_then.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(found.borrow().get_property("value"), Value::string("dark"));

        store.call_method("delete", vec![Value::string("theme")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(document_cookie(), "");
        assert_eq!(
            changes.borrow()[1]
                .get_property("deleted")
                .get_property("length"),
            Value::Number(1.0)
        );
    }

    #[test]
    fn cookie_state_is_isolated_by_active_origin() {
        reset();
        set_active_origin("https://first.example");
        set_document_cookie("session=first; Path=/");
        assert_eq!(document_cookie(), "session=first");

        set_active_origin("https://second.example");
        assert_eq!(document_cookie(), "");
        set_document_cookie("session=second; Path=/");
        assert_eq!(document_cookie(), "session=second");

        set_active_origin("https://first.example");
        assert_eq!(document_cookie(), "session=first");
        assert_eq!(
            cookie_header_for_origin("https://second.example"),
            "session=second"
        );
    }

    #[test]
    fn cookie_matching_honors_domain_path_secure_and_http_only() {
        reset();
        set_cookie_assignment_for_url(
            "https://api.example.test/maps/bootstrap.js",
            "host=one; Path=/maps",
            true,
        );
        set_cookie_assignment_for_url(
            "https://api.example.test/maps/bootstrap.js",
            "shared=two; Domain=example.test; Path=/; Secure",
            true,
        );
        set_cookie_assignment_for_url(
            "https://api.example.test/maps/bootstrap.js",
            "server=secret; Path=/maps; HttpOnly",
            true,
        );

        assert_eq!(
            cookie_header_for_url("https://api.example.test/maps/tile"),
            "host=one; server=secret; shared=two"
        );
        assert_eq!(
            cookie_header_for_url("https://cdn.example.test/assets/chunk.js"),
            "shared=two"
        );
        assert_eq!(
            cookie_header_for_url("http://cdn.example.test/assets/chunk.js"),
            ""
        );
        set_active_url("https://api.example.test/maps/index.html");
        assert_eq!(document_cookie(), "host=one; shared=two");
    }

    #[test]
    fn max_age_deletes_the_matching_cookie_and_invalid_domains_are_rejected() {
        reset();
        let url = "https://maps.example.test/app/index.html";
        set_cookie_assignment_for_url(url, "session=live; Path=/app", true);
        set_cookie_assignment_for_url(url, "escaped=no; Domain=unrelated.test; Path=/", true);
        assert_eq!(
            cookie_header_for_url("https://maps.example.test/app/chunk.js"),
            "session=live"
        );
        set_cookie_assignment_for_url(url, "session=gone; Path=/app; Max-Age=0", true);
        assert_eq!(
            cookie_header_for_url("https://maps.example.test/app/chunk.js"),
            ""
        );
    }

    #[test]
    fn cookie_limits_reject_oversized_pairs_and_evict_oldest_entries() {
        reset();
        let oversized = "x".repeat(MAX_COOKIE_NAME_VALUE_BYTES);
        set_cookie_assignment_for_url(
            "https://maps.example.test/",
            &format!("name={oversized}; Path=/"),
            true,
        );
        assert_eq!(cookie_header_for_url("https://maps.example.test/"), "");

        let cookie = |name: String, domain: String| CookieEntry {
            name,
            value: "value".to_string(),
            domain,
            host_only: true,
            path: "/".to_string(),
            secure: false,
            http_only: false,
            expires_at: None,
            same_site: CookieSameSite::Lax,
        };
        let mut per_site = (0..=MAX_COOKIES_PER_SITE)
            .map(|index| {
                cookie(
                    format!("cookie-{index}"),
                    format!("host-{index}.example.com"),
                )
            })
            .collect::<Vec<_>>();
        let evicted = enforce_cookie_quotas(&mut per_site);
        assert_eq!(per_site.len(), MAX_COOKIES_PER_SITE);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].name, "cookie-0");

        let mut global = (0..=MAX_COOKIES_TOTAL)
            .map(|index| cookie(format!("cookie-{index}"), format!("site-{index}.com")))
            .collect::<Vec<_>>();
        let evicted = enforce_cookie_quotas(&mut global);
        assert_eq!(global.len(), MAX_COOKIES_TOTAL);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].name, "cookie-0");
    }

    #[test]
    fn cookie_date_accepts_http_date_variants_and_rejects_invalid_dates() {
        let imf = parse_cookie_date("Wed, 09 Jun 2038 10:18:14 GMT").expect("IMF-fixdate");
        let rfc850 = parse_cookie_date("Wednesday, 09-Jun-38 10:18:14 GMT").expect("RFC 850 date");
        let asctime = parse_cookie_date("Wed Jun  9 10:18:14 2038").expect("asctime date");
        assert_eq!(imf, rfc850);
        assert_eq!(imf, asctime);
        assert!(parse_cookie_date("Wed, 31 Feb 2038 10:18:14 GMT").is_none());
        assert!(parse_cookie_date("Wed, 09 Jun 1500 10:18:14 GMT").is_none());
    }

    #[test]
    fn expires_deletes_cookies_and_max_age_takes_precedence() {
        reset();
        let url = "https://maps.example.test/app/index.html";
        set_cookie_assignment_for_url(
            url,
            "session=future; Path=/app; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );
        assert_eq!(
            cookie_header_for_url("https://maps.example.test/app/chunk.js"),
            "session=future"
        );

        set_cookie_assignment_for_url(
            url,
            "session=deleted; Path=/app; Expires=Wed, 21 Oct 2015 07:28:00 GMT",
            true,
        );
        assert_eq!(
            cookie_header_for_url("https://maps.example.test/app/chunk.js"),
            ""
        );

        set_cookie_assignment_for_url(
            url,
            "override=live; Path=/app; Max-Age=60; Expires=Wed, 21 Oct 2015 07:28:00 GMT",
            true,
        );
        assert_eq!(
            cookie_header_for_url("https://maps.example.test/app/chunk.js"),
            "override=live"
        );
        set_cookie_assignment_for_url(
            url,
            "override=gone; Path=/app; Max-Age=0; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );
        assert_eq!(
            cookie_header_for_url("https://maps.example.test/app/chunk.js"),
            ""
        );
    }

    #[test]
    fn same_site_none_requires_secure_and_cookie_values_expose_attributes() {
        reset();
        let url = "https://maps.example.test/app/index.html";
        set_cookie_assignment_for_url(url, "rejected=value; Path=/; SameSite=None", true);
        assert_eq!(cookie_header_for_url(url), "");

        set_cookie_assignment_for_url(
            url,
            "accepted=value; Path=/; SameSite=None; Secure; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );
        let cookie = visible_cookies_for_url(url, true)
            .into_iter()
            .next()
            .expect("secure SameSite=None cookie");
        let value = cookie_value(&cookie);
        assert_eq!(value.get_property("sameSite"), Value::string("none"));
        assert!(value.get_property("expires").to_number() > 0.0);
        assert_eq!(cookie_header_for_url(url), "accepted=value");
    }

    #[test]
    fn same_site_request_context_uses_schemeful_registrable_sites() {
        reset();
        let resource = "https://cdn.example.co.uk/assets/module.js";
        set_cookie_assignment_for_url(
            resource,
            "strict=yes; Path=/; SameSite=Strict; Secure",
            true,
        );
        set_cookie_assignment_for_url(resource, "lax=yes; Path=/; SameSite=Lax; Secure", true);
        set_cookie_assignment_for_url(resource, "none=yes; Path=/; SameSite=None; Secure", true);
        let snapshot = snapshot();

        assert_eq!(
            snapshot.header_for_request(
                resource,
                &CookieRequestContext::subresource("https://app.example.co.uk/index.html")
            ),
            "strict=yes; lax=yes; none=yes"
        );
        assert_eq!(
            snapshot.header_for_request(
                resource,
                &CookieRequestContext::subresource("https://app.other.co.uk/index.html")
            ),
            "none=yes"
        );
        assert_eq!(
            snapshot.header_for_request(
                resource,
                &CookieRequestContext::subresource("http://app.example.co.uk/index.html")
            ),
            "none=yes"
        );
        assert_eq!(
            snapshot.header_for_request(
                resource,
                &CookieRequestContext::top_level_navigation(
                    "https://app.other.co.uk/index.html",
                    true
                )
            ),
            "lax=yes; none=yes"
        );
        assert_eq!(
            snapshot.header_for_request(
                resource,
                &CookieRequestContext::top_level_navigation(
                    "https://app.other.co.uk/index.html",
                    false
                )
            ),
            "none=yes"
        );
    }

    #[test]
    fn domain_attribute_rejects_public_suffixes() {
        reset();
        let url = "https://maps.example.co.uk/app/index.html";
        set_cookie_assignment_for_url(url, "rejected=yes; Domain=co.uk; Path=/", true);
        set_cookie_assignment_for_url(url, "accepted=yes; Domain=example.co.uk; Path=/", true);
        assert_eq!(
            cookie_header_for_url("https://tiles.example.co.uk/"),
            "accepted=yes"
        );

        set_cookie_assignment_for_url(
            "https://co.uk/",
            "host-only=yes; Domain=co.uk; Path=/",
            true,
        );
        assert_eq!(cookie_header_for_url("https://co.uk/"), "host-only=yes");
        assert_eq!(cookie_header_for_url("https://child.co.uk/"), "");
    }

    #[test]
    fn schemeful_site_handles_ip_literals_and_local_hosts() {
        assert_eq!(
            schemeful_site("https://tiles.example.co.uk:8443/path"),
            Some("https://example.co.uk".to_string())
        );
        assert_eq!(
            schemeful_site("http://127.0.0.1:8080/path"),
            Some("http://127.0.0.1".to_string())
        );
        assert_eq!(
            schemeful_site("http://[::1]:8080/path"),
            Some("http://[::1]".to_string())
        );
        assert_eq!(
            schemeful_site("http://localhost:8080/path"),
            Some("http://localhost".to_string())
        );
    }

    #[test]
    fn corp_same_site_uses_registrable_domains_and_blocks_secure_downgrades() {
        assert!(urls_are_corp_same_site(
            "https://maps.example.co.uk/app",
            "https://tiles.example.co.uk/resource"
        ));
        assert!(urls_are_corp_same_site(
            "http://127.0.0.1:3000/app",
            "http://127.0.0.1:4000/resource"
        ));
        assert!(!urls_are_corp_same_site(
            "http://maps.example.test/app",
            "https://tiles.example.test/resource"
        ));
        assert!(!urls_are_corp_same_site(
            "https://example.test/app",
            "https://example.invalid/resource"
        ));
    }

    #[test]
    fn redirect_cookie_snapshot_uses_the_same_expiry_parser() {
        reset();
        let url = "https://maps.example.test/redirect/start.js";
        let mut snapshot = snapshot();
        snapshot.apply_http_set_cookie(
            url,
            "hop=ready; Path=/redirect; Expires=Wednesday, 09-Jun-38 10:18:14 GMT",
        );
        assert_eq!(snapshot.header_for_url(url), "hop=ready");
        snapshot.apply_http_set_cookie(
            url,
            "hop=gone; Path=/redirect; Expires=Wed Jun  9 10:18:14 2010",
        );
        assert_eq!(snapshot.header_for_url(url), "");
    }

    #[test]
    fn persistent_jar_survives_reload_but_session_cookie_does_not() {
        reset();
        let directory = tempfile::tempdir().expect("temporary cookie directory");
        set_persistence_dir(directory.path().to_path_buf()).expect("bind persistent jar");
        let url = "https://maps.example.test/app/index.html";

        set_cookie_assignment_for_url(
            url,
            "persisted=yes; Path=/; HttpOnly; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );
        set_cookie_assignment_for_url(url, "session=no; Path=/", true);
        assert_eq!(cookie_header_for_url(url), "persisted=yes; session=no");
        let encoded =
            std::fs::read_to_string(directory.path().join("cookies.json")).expect("cookie file");
        assert!(encoded.contains("\"persisted\""));
        assert!(!encoded.contains("\"session\""));

        reset();
        set_persistence_dir(directory.path().to_path_buf()).expect("reload persistent jar");
        assert_eq!(cookie_header_for_url(url), "persisted=yes");
        set_active_url(url);
        assert_eq!(document_cookie(), "");
        reset();
    }

    #[test]
    fn persistent_cookie_deletion_survives_reload() {
        reset();
        let directory = tempfile::tempdir().expect("temporary cookie directory");
        set_persistence_dir(directory.path().to_path_buf()).expect("bind persistent jar");
        let url = "https://maps.example.test/app/index.html";
        set_cookie_assignment_for_url(
            url,
            "persisted=yes; Path=/; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );
        set_cookie_assignment_for_url(url, "persisted=gone; Path=/; Max-Age=0", true);

        reset();
        set_persistence_dir(directory.path().to_path_buf()).expect("reload persistent jar");
        assert_eq!(cookie_header_for_url(url), "");
        reset();
    }

    #[test]
    fn encrypted_persistent_profiles_are_isolated_and_fail_closed() {
        reset();
        let directory = tempfile::tempdir().expect("temporary encrypted cookie directory");
        let protector: Arc<dyn CookiePersistenceProtector> =
            Arc::new(TestCookieProtector { key: 0xa5 });
        let alpha_url = "https://alpha.example.test/app";
        let beta_url = "https://beta.example.test/app";

        set_encrypted_persistence_profile(
            directory.path().to_path_buf(),
            "alpha",
            Arc::clone(&protector),
        )
        .expect("bind alpha profile");
        set_cookie_assignment_for_url(
            alpha_url,
            "alpha-secret=one; Path=/; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );
        let alpha_path = encrypted_profile_cookie_path(directory.path(), "alpha");
        let alpha_encoded = std::fs::read(&alpha_path).expect("encrypted alpha cookie file");
        assert!(alpha_encoded.starts_with(PROTECTED_COOKIE_MAGIC));
        assert!(
            !alpha_encoded
                .windows(b"alpha-secret".len())
                .any(|window| window == b"alpha-secret")
        );

        set_encrypted_persistence_profile(
            directory.path().to_path_buf(),
            "beta",
            Arc::clone(&protector),
        )
        .expect("bind beta profile");
        assert_eq!(cookie_header_for_url(alpha_url), "");
        set_cookie_assignment_for_url(
            beta_url,
            "beta-secret=two; Path=/; Expires=Wed, 09 Jun 2038 10:18:14 GMT",
            true,
        );

        set_encrypted_persistence_profile(
            directory.path().to_path_buf(),
            "alpha",
            Arc::clone(&protector),
        )
        .expect("reload alpha profile");
        assert_eq!(cookie_header_for_url(alpha_url), "alpha-secret=one");
        assert_eq!(cookie_header_for_url(beta_url), "");

        set_cookie_assignment_for_url(alpha_url, "live=session; Path=/", true);
        let mut corrupted = std::fs::read(&alpha_path).expect("read alpha ciphertext");
        *corrupted.last_mut().expect("alpha ciphertext is non-empty") ^= 1;
        std::fs::write(&alpha_path, corrupted).expect("corrupt alpha ciphertext");
        assert!(
            set_encrypted_persistence_profile(
                directory.path().to_path_buf(),
                "alpha",
                Arc::clone(&protector),
            )
            .is_err()
        );
        assert_eq!(
            cookie_header_for_url(alpha_url),
            "alpha-secret=one; live=session"
        );

        let plaintext_profile_path = encrypted_profile_cookie_path(directory.path(), "plaintext");
        std::fs::create_dir_all(
            plaintext_profile_path
                .parent()
                .expect("plaintext profile parent"),
        )
        .expect("create plaintext profile directory");
        std::fs::write(
            &plaintext_profile_path,
            br#"{"schema_version":1,"cookies":[]}"#,
        )
        .expect("write plaintext downgrade fixture");
        assert!(
            set_encrypted_persistence_profile(
                directory.path().to_path_buf(),
                "plaintext",
                protector,
            )
            .is_err()
        );
        assert_eq!(
            cookie_header_for_url(alpha_url),
            "alpha-secret=one; live=session"
        );
        reset();
    }

    #[test]
    fn navigation_reset_preserves_and_rematches_cookie_jar() {
        reset();
        let first = "https://first.example.test/app/index.html";
        set_active_url(first);
        set_document_cookie("session=first; Path=/");
        assert_eq!(document_cookie(), "session=first");

        crate::jsdom::reset_bridge();
        set_active_url(first);
        assert_eq!(document_cookie(), "session=first");
        set_active_url("https://second.example.test/app/index.html");
        assert_eq!(document_cookie(), "");
        reset();
    }

    #[test]
    fn corrupt_persistent_file_does_not_replace_live_jar() {
        reset();
        let url = "https://maps.example.test/app/index.html";
        set_cookie_assignment_for_url(url, "live=yes; Path=/", true);
        let directory = tempfile::tempdir().expect("temporary cookie directory");
        std::fs::write(directory.path().join("cookies.json"), b"not-json")
            .expect("write corrupt cookie file");

        assert!(set_persistence_dir(directory.path().to_path_buf()).is_err());
        assert_eq!(cookie_header_for_url(url), "live=yes");
        reset();
    }
}
