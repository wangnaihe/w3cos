//! Crate and symbol attribution from an unstripped object file.
//!
//! Sizes come from the symbol table. They do not sum to the file size: padding,
//! headers, and data without a symbol stay outside the table. This is the same
//! class of measurement as `cargo bloat`.

use object::{Object, ObjectSymbol, SymbolKind};
use serde::Serialize;
use std::collections::BTreeMap;

pub const TOP_SYMBOLS: usize = 40;

/// Feature buckets whose crate names we can recognize in a Rust symbol table.
/// A crate may match more than one bucket. Buckets are symbol-table bytes, not
/// a separate link of that feature.
const FEATURE_CRATES: &[(&str, &[&str])] = &[
    (
        "dynamic-js",
        &[
            "w3cos_compiler",
            "w3cos_ir",
            "w3cos_vm",
            "swc_",
            "wuff",
            "brotli",
        ],
    ),
    ("web-graphics-advanced", &["naga"]),
    (
        "gpu",
        &[
            "vello",
            "vello_encoding",
            "wgpu",
            "wgpu_core",
            "wgpu_hal",
            "wgpu_types",
            "skrifa",
        ],
    ),
    ("skia", &["skia_safe", "skia_bindings"]),
    // `tiny_skia` also comes from resvg, so it is not a cpu-render marker.
    ("cpu-render", &["softbuffer"]),
    (
        "web-media-advanced",
        &["cpal", "nokhwa", "nokhwa_bindings_macos"],
    ),
    ("web-gamepad", &["gilrs"]),
    ("web-device-access", &["hidapi", "nusb"]),
    ("web-serial", &["serialport"]),
    ("host-tray", &["ksni", "tray_icon"]),
    (
        "host-keyring",
        &[
            "=keyring",
            "secret_service",
            "zbus_secret_service_keyring_store",
        ],
    ),
    (
        "image-codecs",
        &[
            "image",
            "png",
            "zune_",
            "jpeg_decoder",
            "webp",
            "gif",
            "bmp_rust",
        ],
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrateBytes {
    pub name: String,
    pub bytes: u64,
    pub symbols: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolBytes {
    pub name: String,
    pub crate_name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeatureBytes {
    pub feature: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attribution {
    pub symbols_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols_path: Option<String>,
    pub symbol_bytes: u64,
    pub symbol_count: u64,
    pub crates: Vec<CrateBytes>,
    pub top_symbols: Vec<SymbolBytes>,
    pub features: Vec<FeatureBytes>,
}

pub fn attribute_object(bytes: &[u8]) -> Attribution {
    let Ok(object) = object::File::parse(bytes) else {
        return empty();
    };
    let mut crates: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut symbols = Vec::new();
    for symbol in object.symbols() {
        if symbol.is_undefined() || symbol.size() == 0 {
            continue;
        }
        match symbol.kind() {
            SymbolKind::Text | SymbolKind::Data | SymbolKind::Tls | SymbolKind::Unknown => {}
            _ => continue,
        }
        let raw = symbol.name().unwrap_or("");
        if raw.is_empty() {
            continue;
        }
        let demangled = demangle(raw);
        let crate_name = crate_name_of(&demangled);
        let entry = crates.entry(crate_name.clone()).or_insert((0, 0));
        entry.0 += symbol.size();
        entry.1 += 1;
        symbols.push(SymbolBytes {
            name: truncate_symbol(&demangled),
            crate_name,
            bytes: symbol.size(),
        });
    }
    if symbols.is_empty() {
        return empty();
    }
    let mut crates: Vec<CrateBytes> = crates
        .into_iter()
        .map(|(name, (bytes, count))| CrateBytes {
            name,
            bytes,
            symbols: count,
        })
        .collect();
    crates.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then(left.name.cmp(&right.name))
    });
    let symbol_bytes = crates.iter().map(|item| item.bytes).sum();
    let symbol_count = crates.iter().map(|item| item.symbols).sum();
    symbols.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then(left.name.cmp(&right.name))
    });
    symbols.truncate(TOP_SYMBOLS);
    let features = feature_bytes(&crates);
    Attribution {
        symbols_present: true,
        symbols_path: None,
        symbol_bytes,
        symbol_count,
        crates,
        top_symbols: symbols,
        features,
    }
}

pub fn crate_name_of(demangled: &str) -> String {
    let mut rest = demangled.trim();
    loop {
        let stripped = rest.trim_start_matches(['<', '[', '(', '{', ':', ' ']);
        if let Some(next) = stripped.strip_prefix("impl ") {
            rest = next;
            continue;
        }
        rest = stripped;
        break;
    }
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .unwrap_or(rest.len());
    if end == 0 {
        return "[other]".to_string();
    }
    let name = &rest[..end];
    // C and linker symbols have no Rust path. Keep them in one bucket so they
    // do not look like a Rust dependency named `memcpy`.
    if demangled.contains("::") || name.contains('_') {
        name.to_string()
    } else {
        "[other]".to_string()
    }
}

pub fn feature_bytes(crates: &[CrateBytes]) -> Vec<FeatureBytes> {
    FEATURE_CRATES
        .iter()
        .map(|(feature, patterns)| FeatureBytes {
            feature: (*feature).to_string(),
            bytes: crates
                .iter()
                .filter(|item| {
                    patterns
                        .iter()
                        .any(|pattern| matches_pattern(&item.name, pattern))
                })
                .map(|item| item.bytes)
                .sum(),
        })
        .collect()
}

fn matches_pattern(crate_name: &str, pattern: &str) -> bool {
    if let Some(exact) = pattern.strip_prefix('=') {
        return crate_name == exact;
    }
    if let Some(prefix) = pattern.strip_suffix('_') {
        crate_name == prefix || crate_name.starts_with(pattern)
    } else {
        crate_name == pattern || crate_name.starts_with(&format!("{pattern}_"))
    }
}

fn demangle(raw: &str) -> String {
    match rustc_demangle::try_demangle(raw) {
        Ok(demangled) => format!("{demangled:#}"),
        Err(_) => raw.to_string(),
    }
}

fn truncate_symbol(name: &str) -> String {
    const MAX: usize = 240;
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let truncated: String = name.chars().take(MAX).collect();
    format!("{truncated}…")
}

fn empty() -> Attribution {
    Attribution {
        symbols_present: false,
        symbols_path: None,
        symbol_bytes: 0,
        symbol_count: 0,
        crates: Vec::new(),
        top_symbols: Vec::new(),
        features: feature_bytes(&[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_reads_rust_paths() {
        assert_eq!(crate_name_of("w3cos_runtime::jsdom::body"), "w3cos_runtime");
        assert_eq!(
            crate_name_of("<w3cos_runtime::Foo as core::fmt::Debug>::fmt"),
            "w3cos_runtime"
        );
        assert_eq!(crate_name_of("memcpy"), "[other]");
        assert_eq!(crate_name_of("std::vec::Vec<u8>::push"), "std");
    }

    #[test]
    fn feature_buckets_match_prefixes_without_swallowing_neighbors() {
        let crates = vec![
            CrateBytes {
                name: "swc_ecma_parser".into(),
                bytes: 100,
                symbols: 1,
            },
            CrateBytes {
                name: "w3cos_vm".into(),
                bytes: 40,
                symbols: 1,
            },
            CrateBytes {
                name: "image".into(),
                bytes: 7,
                symbols: 1,
            },
            CrateBytes {
                name: "imagery".into(),
                bytes: 9,
                symbols: 1,
            },
            CrateBytes {
                name: "naga".into(),
                bytes: 3,
                symbols: 1,
            },
        ];
        let features = feature_bytes(&crates);
        let dynamic = features
            .iter()
            .find(|item| item.feature == "dynamic-js")
            .unwrap();
        assert_eq!(dynamic.bytes, 140);
        let codecs = features
            .iter()
            .find(|item| item.feature == "image-codecs")
            .unwrap();
        assert_eq!(codecs.bytes, 7);
        let graphics = features
            .iter()
            .find(|item| item.feature == "web-graphics-advanced")
            .unwrap();
        assert_eq!(graphics.bytes, 3);
        let keyring = feature_bytes(&[
            CrateBytes {
                name: "keyring".into(),
                bytes: 10,
                symbols: 1,
            },
            CrateBytes {
                name: "keyring_core".into(),
                bytes: 4,
                symbols: 1,
            },
        ]);
        assert_eq!(
            keyring
                .iter()
                .find(|item| item.feature == "host-keyring")
                .unwrap()
                .bytes,
            10
        );
    }

    #[test]
    fn attributes_the_current_test_executable() {
        let path = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let attribution = attribute_object(&bytes);
        assert!(attribution.symbols_present);
        assert!(attribution.symbol_bytes > 0);
        assert!(!attribution.crates.is_empty());
        assert!(!attribution.top_symbols.is_empty());
        assert!(
            attribution
                .features
                .iter()
                .any(|item| item.feature == "dynamic-js")
        );
    }
}
