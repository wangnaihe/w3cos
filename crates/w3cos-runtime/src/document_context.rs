//! Embedder-owned document URL configuration for packaged applications.

/// Validate and normalize the HTTP(S) base URL used by browser-relative APIs.
pub fn normalize_document_base_url(value: &str) -> Result<String, String> {
    let mut url = url::Url::parse(value)
        .map_err(|error| format!("document_base_url must be an absolute URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "document_base_url must use http or https, got {}",
            url.scheme()
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("document_base_url must not contain credentials".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("document_base_url must not contain a query or fragment".to_string());
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url.to_string())
}

/// Configure the initial document URL before application JavaScript executes.
///
/// Packaged modules retain their `w3cos://` identity, while browser-relative
/// network APIs, `window.location`, cookies and same-origin checks use this URL.
pub fn configure_document_base_url(value: &str) -> Result<String, String> {
    let normalized = normalize_document_base_url(value)?;
    crate::history::location_replace(&normalized);
    crate::cookie_store_web::set_active_url(&normalized);
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_http_document_base_urls() {
        assert_eq!(
            normalize_document_base_url("https://app.example.test/mobile").unwrap(),
            "https://app.example.test/mobile/"
        );
        assert_eq!(
            normalize_document_base_url("http://localhost:5174/").unwrap(),
            "http://localhost:5174/"
        );
    }

    #[test]
    fn rejects_non_network_or_credentialed_document_base_urls() {
        for value in [
            "w3cos://app/",
            "https://user:password@example.test/",
            "https://example.test/?mode=app",
            "https://example.test/#app",
        ] {
            assert!(
                normalize_document_base_url(value).is_err(),
                "accepted {value}"
            );
        }
    }

    #[test]
    fn configured_base_drives_location_and_relative_fetch_resolution() {
        crate::history::reset();
        crate::cookie_store_web::reset_document_context();

        configure_document_base_url("https://app.example.test/mobile").unwrap();

        assert_eq!(
            crate::history::get_href(),
            "https://app.example.test/mobile/"
        );
        assert_eq!(
            crate::fetch::resolve_page_fetch_url("/api/session").unwrap(),
            "https://app.example.test/api/session"
        );
        assert_eq!(
            crate::fetch::resolve_page_fetch_url("asset.json").unwrap(),
            "https://app.example.test/mobile/asset.json"
        );

        crate::cookie_store_web::reset_document_context();
        crate::history::reset();
    }
}
