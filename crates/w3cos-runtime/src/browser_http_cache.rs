use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const CACHE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone)]
pub(crate) struct CachePolicy {
    pub(crate) directory: Option<PathBuf>,
    pub(crate) max_entries: usize,
    pub(crate) max_bytes: usize,
    pub(crate) max_body_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheKey {
    pub(crate) request_url: String,
    pub(crate) partition: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VaryValue {
    name: String,
    value_sha256: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedResponse {
    schema_version: u32,
    request_url: String,
    partition: String,
    pub(crate) final_url: String,
    pub(crate) status: u16,
    pub(crate) status_text: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Vec<u8>,
    body_hash: u64,
    vary: Vec<VaryValue>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StoreOutcome {
    pub(crate) wrote: bool,
    pub(crate) response_evictions: u64,
    pub(crate) other_evictions: u64,
}

impl CachedResponse {
    pub(crate) fn from_network(
        key: &CacheKey,
        final_url: String,
        status: u16,
        status_text: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            request_url: key.request_url.clone(),
            partition: key.partition.clone(),
            final_url,
            status,
            status_text,
            headers: cacheable_response_headers(&headers),
            body_hash: stable_hash(&body),
            body,
            vary: Vec::new(),
        }
    }
}

pub(crate) fn cache_identity(key: &CacheKey) -> u64 {
    let mut hash = stable_hash(key.request_url.as_bytes());
    hash = extend_hash(hash, &CACHE_SCHEMA_VERSION.to_le_bytes());
    extend_hash(hash, key.partition.as_bytes())
}

pub(crate) fn cache_path(policy: &CachePolicy, key: &CacheKey) -> Option<PathBuf> {
    policy
        .directory
        .as_ref()
        .map(|directory| directory.join(format!("{:016x}.response.json", cache_identity(key))))
}

pub(crate) fn load(
    policy: &CachePolicy,
    key: &CacheKey,
    request_headers: &HashMap<String, String>,
) -> std::io::Result<Option<CachedResponse>> {
    let Some(path) = cache_path(policy, key) else {
        return Ok(None);
    };
    if policy.max_entries == 0 || policy.max_bytes == 0 {
        return Ok(None);
    }
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.len() > policy.max_bytes as u64 {
        return Err(std::io::Error::other(
            "persistent HTTP response exceeds cache byte budget",
        ));
    }
    let encoded = std::fs::read(path)?;
    let cached: CachedResponse = serde_json::from_slice(&encoded).map_err(std::io::Error::other)?;
    if cached.schema_version != CACHE_SCHEMA_VERSION
        || cached.request_url != key.request_url
        || cached.partition != key.partition
        || cached.body.len() > policy.max_body_bytes
        || cached.body_hash != stable_hash(&cached.body)
        || !url::Url::parse(&cached.final_url)
            .map(|url| matches!(url.scheme(), "http" | "https"))
            .unwrap_or(false)
        || (header_value(&cached.headers, "etag").is_none()
            && header_value(&cached.headers, "last-modified").is_none())
    {
        return Err(std::io::Error::other(
            "persistent HTTP response metadata is invalid",
        ));
    }
    if !cached.vary.iter().all(|vary| {
        vary_value_hash(request_header_value(request_headers, &vary.name).unwrap_or_default())
            == vary.value_sha256
    }) {
        return Ok(None);
    }
    Ok(Some(cached))
}

pub(crate) fn store(
    policy: &CachePolicy,
    key: &CacheKey,
    request_headers: &HashMap<String, String>,
    mut response: CachedResponse,
    allow_vary: bool,
) -> std::io::Result<StoreOutcome> {
    let Some(path) = cache_path(policy, key) else {
        return Ok(StoreOutcome::default());
    };
    let vary_names = vary_names(&response.headers)?;
    let has_validator = header_value(&response.headers, "etag").is_some()
        || header_value(&response.headers, "last-modified").is_some();
    if !has_validator
        || cache_control_has_directive(&response.headers, "no-store")
        || vary_names.is_none()
        || (!allow_vary && vary_names.as_ref().is_some_and(|names| !names.is_empty()))
    {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        return Ok(StoreOutcome::default());
    }
    if policy.max_entries == 0
        || policy.max_bytes == 0
        || response.body.len() > policy.max_body_bytes
    {
        remove_cache_file(&path)?;
        return Ok(StoreOutcome::default());
    }
    response.vary = vary_names
        .unwrap_or_default()
        .into_iter()
        .map(|name| VaryValue {
            value_sha256: vary_value_hash(
                request_header_value(request_headers, &name).unwrap_or_default(),
            ),
            name,
        })
        .collect();
    response.body_hash = stable_hash(&response.body);
    let encoded = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
    if encoded.len() > policy.max_bytes {
        remove_cache_file(&path)?;
        return Ok(StoreOutcome::default());
    }
    let Some(directory) = path.parent() else {
        return Ok(StoreOutcome::default());
    };
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&encoded)?;
    temporary.flush()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    let (response_evictions, other_evictions) =
        prune(directory, policy.max_entries, policy.max_bytes as u64)?;
    Ok(StoreOutcome {
        wrote: true,
        response_evictions,
        other_evictions,
    })
}

fn remove_cache_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn add_revalidation_headers(
    headers: &mut HashMap<String, String>,
    cached: &CachedResponse,
) {
    if let Some(etag) = header_value(&cached.headers, "etag") {
        headers.insert("If-None-Match".to_string(), etag.to_string());
    }
    if let Some(last_modified) = header_value(&cached.headers, "last-modified") {
        headers.insert("If-Modified-Since".to_string(), last_modified.to_string());
    }
}

pub(crate) fn merge_not_modified(
    mut cached: CachedResponse,
    final_url: &str,
    revalidated_headers: &HashMap<String, String>,
) -> std::io::Result<CachedResponse> {
    if final_url != cached.final_url {
        return Err(std::io::Error::other(format!(
            "HTTP 304 final URL changed from {} to {final_url}",
            cached.final_url
        )));
    }
    merge_response_headers(&mut cached.headers, revalidated_headers);
    cached.status = 200;
    cached.status_text = "OK (revalidated)".to_string();
    Ok(cached)
}

fn prune(directory: &Path, max_entries: usize, max_bytes: u64) -> std::io::Result<(u64, u64)> {
    let mut files = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_string();
            if !name.ends_with(".response.json") && !name.ends_with(".w3ir.json") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((
                path,
                name.ends_with(".response.json"),
                metadata.len(),
                metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            ))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        left.3
            .cmp(&right.3)
            .then_with(|| left.0.as_os_str().cmp(right.0.as_os_str()))
    });
    let mut bytes = files
        .iter()
        .fold(0_u64, |total, (_, _, size, _)| total.saturating_add(*size));
    let mut entries = files.len();
    let mut response_evictions = 0_u64;
    let mut other_evictions = 0_u64;
    for (path, is_response, size, _) in files {
        if entries <= max_entries && bytes <= max_bytes {
            break;
        }
        std::fs::remove_file(path)?;
        entries = entries.saturating_sub(1);
        bytes = bytes.saturating_sub(size);
        if is_response {
            response_evictions = response_evictions.saturating_add(1);
        } else {
            other_evictions = other_evictions.saturating_add(1);
        }
    }
    Ok((response_evictions, other_evictions))
}

fn vary_names(headers: &HashMap<String, String>) -> std::io::Result<Option<Vec<String>>> {
    let Some(vary) = header_value(headers, "vary") else {
        return Ok(Some(Vec::new()));
    };
    let mut names = vary
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if names.iter().any(|name| name == "*") {
        return Ok(None);
    }
    names.sort();
    names.dedup();
    if names.iter().any(|name| !valid_header_name(name)) {
        return Err(std::io::Error::other("invalid Vary response header"));
    }
    Ok(Some(names))
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn cache_control_has_directive(headers: &HashMap<String, String>, expected: &str) -> bool {
    header_value(headers, "cache-control").is_some_and(|value| {
        value
            .split(',')
            .any(|directive| directive.trim().eq_ignore_ascii_case(expected))
    })
}

fn cacheable_response_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    headers
        .iter()
        .filter(|(name, _)| {
            ![
                "connection",
                "keep-alive",
                "proxy-authenticate",
                "proxy-authorization",
                "set-cookie",
                "set-cookie2",
                "te",
                "trailer",
                "transfer-encoding",
                "upgrade",
            ]
            .iter()
            .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn merge_response_headers(
    cached: &mut HashMap<String, String>,
    revalidated: &HashMap<String, String>,
) {
    for (name, value) in revalidated {
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("set-cookie")
            || name.eq_ignore_ascii_case("set-cookie2")
        {
            continue;
        }
        if let Some(existing) = cached
            .keys()
            .find(|candidate| candidate.eq_ignore_ascii_case(name))
            .cloned()
        {
            cached.remove(&existing);
        }
        cached.insert(name.clone(), value.clone());
    }
}

fn request_header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn vary_value_hash(value: &str) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, value.as_bytes())
        .as_ref()
        .to_vec()
}

fn stable_hash(bytes: &[u8]) -> u64 {
    extend_hash(0xcbf29ce484222325_u64, bytes)
}

fn extend_hash(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(directory: &Path) -> CachePolicy {
        CachePolicy {
            directory: Some(directory.to_path_buf()),
            max_entries: 16,
            max_bytes: 1024 * 1024,
            max_body_bytes: 1024,
        }
    }

    fn key() -> CacheKey {
        CacheKey {
            request_url: "https://example.test/resource".to_string(),
            partition: "page:https://app.test:include".to_string(),
        }
    }

    fn response(key: &CacheKey, headers: HashMap<String, String>) -> CachedResponse {
        CachedResponse::from_network(
            key,
            key.request_url.clone(),
            200,
            "OK".to_string(),
            headers,
            vec![0, 159, 146, 150, 255],
        )
    }

    #[test]
    fn binary_response_round_trips_without_persisting_cookies() {
        let directory = tempfile::tempdir().unwrap();
        let policy = policy(directory.path());
        let key = key();
        let headers = HashMap::from([
            ("ETag".to_string(), "\"binary-v1\"".to_string()),
            ("Content-Type".to_string(), "image/webp".to_string()),
            ("Set-Cookie".to_string(), "secret=value".to_string()),
        ]);

        let outcome = store(
            &policy,
            &key,
            &HashMap::new(),
            response(&key, headers),
            true,
        )
        .unwrap();
        let loaded = load(&policy, &key, &HashMap::new()).unwrap().unwrap();

        assert!(outcome.wrote);
        assert_eq!(loaded.body, vec![0, 159, 146, 150, 255]);
        assert_eq!(
            header_value(&loaded.headers, "content-type"),
            Some("image/webp")
        );
        assert_eq!(header_value(&loaded.headers, "set-cookie"), None);
        let encoded = std::fs::read_to_string(cache_path(&policy, &key).unwrap()).unwrap();
        assert!(!encoded.contains("secret=value"));
    }

    #[test]
    fn vary_matches_the_request_headers_used_to_store_the_response() {
        let directory = tempfile::tempdir().unwrap();
        let policy = policy(directory.path());
        let key = key();
        let headers = HashMap::from([
            ("ETag".to_string(), "\"vary-v1\"".to_string()),
            ("Vary".to_string(), "Accept-Language, X-Theme".to_string()),
        ]);
        let request_headers = HashMap::from([
            ("accept-language".to_string(), "zh-CN".to_string()),
            ("X-Theme".to_string(), "private-dark-theme".to_string()),
        ]);
        store(
            &policy,
            &key,
            &request_headers,
            response(&key, headers),
            true,
        )
        .unwrap();

        assert!(load(&policy, &key, &request_headers).unwrap().is_some());
        let mismatch = HashMap::from([
            ("Accept-Language".to_string(), "en-US".to_string()),
            ("x-theme".to_string(), "private-dark-theme".to_string()),
        ]);
        assert!(load(&policy, &key, &mismatch).unwrap().is_none());
        let encoded = std::fs::read_to_string(cache_path(&policy, &key).unwrap()).unwrap();
        assert!(!encoded.contains("private-dark-theme"));
    }

    #[test]
    fn no_store_and_vary_star_remove_an_existing_entry() {
        let directory = tempfile::tempdir().unwrap();
        let policy = policy(directory.path());
        let key = key();
        let cache_path = cache_path(&policy, &key).unwrap();
        let cacheable = HashMap::from([("ETag".to_string(), "\"v1\"".to_string())]);

        store(
            &policy,
            &key,
            &HashMap::new(),
            response(&key, cacheable.clone()),
            true,
        )
        .unwrap();
        assert!(cache_path.exists());

        let no_store = HashMap::from([
            ("ETag".to_string(), "\"v2\"".to_string()),
            ("Cache-Control".to_string(), "private, no-store".to_string()),
        ]);
        let outcome = store(
            &policy,
            &key,
            &HashMap::new(),
            response(&key, no_store),
            true,
        )
        .unwrap();
        assert!(!outcome.wrote);
        assert!(!cache_path.exists());

        store(
            &policy,
            &key,
            &HashMap::new(),
            response(&key, cacheable),
            true,
        )
        .unwrap();
        let vary_star = HashMap::from([
            ("ETag".to_string(), "\"v3\"".to_string()),
            ("Vary".to_string(), "*".to_string()),
        ]);
        store(
            &policy,
            &key,
            &HashMap::new(),
            response(&key, vary_star),
            true,
        )
        .unwrap();
        assert!(!cache_path.exists());
    }

    #[test]
    fn not_modified_merges_safe_headers_and_preserves_binary_body() {
        let key = key();
        let cached = response(
            &key,
            HashMap::from([
                ("ETag".to_string(), "\"v1\"".to_string()),
                ("Content-Type".to_string(), "image/webp".to_string()),
            ]),
        );
        let merged = merge_not_modified(
            cached,
            &key.request_url,
            &HashMap::from([
                ("ETag".to_string(), "\"v2\"".to_string()),
                ("Content-Length".to_string(), "0".to_string()),
                ("Set-Cookie".to_string(), "secret=value".to_string()),
            ]),
        )
        .unwrap();

        assert_eq!(merged.status, 200);
        assert_eq!(merged.status_text, "OK (revalidated)");
        assert_eq!(merged.body, vec![0, 159, 146, 150, 255]);
        assert_eq!(header_value(&merged.headers, "etag"), Some("\"v2\""));
        assert_eq!(header_value(&merged.headers, "content-length"), None);
        assert_eq!(header_value(&merged.headers, "set-cookie"), None);
    }
}
