//! Harness sentences from the shell Fluent packs.
//!
//! The shell's locale module is what the window paints. This copy reads the
//! same files so a tool host can speak before that window exists. Lookup is
//! the active pack, then zh-CN, then the key.

use std::collections::BTreeMap;
use std::sync::OnceLock;

const ZH_SRC: &str = include_str!("../../../locales/zh-CN.ftl");
const EN_SRC: &str = include_str!("../../../locales/en.ftl");

pub fn resolve_tag(raw: &str) -> String {
    let token = raw
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    let lower = token.to_ascii_lowercase();
    if lower == "en" || lower.starts_with("en-") {
        "en".to_string()
    } else {
        "zh-CN".to_string()
    }
}

pub fn line(locale: &str, key: &str, args: &[(&str, &str)]) -> String {
    let packs = packs();
    let locale = resolve_tag(locale);
    let pattern = packs
        .get(&locale)
        .and_then(|pack| pack.get(key))
        .or_else(|| packs.get("zh-CN").and_then(|pack| pack.get(key)));
    match pattern {
        Some(pattern) => substitute(pattern, args),
        None => key.to_string(),
    }
}

fn packs() -> &'static BTreeMap<String, BTreeMap<String, String>> {
    static PACKS: OnceLock<BTreeMap<String, BTreeMap<String, String>>> = OnceLock::new();
    PACKS.get_or_init(|| {
        let mut map = BTreeMap::new();
        map.insert("zh-CN".to_string(), parse_ftl(ZH_SRC));
        map.insert("en".to_string(), parse_ftl(EN_SRC));
        map
    })
}

fn parse_ftl(src: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in src.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim().trim_end_matches('\r');
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            continue;
        }
        let value = value.trim();
        if !value.is_empty() {
            map.insert(name.to_string(), value.to_string());
        }
    }
    map
}

fn substitute(text: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inside = rest[start + 1..start + end].trim();
        if let Some(name) = inside.strip_prefix('$') {
            let name = name.trim();
            let value = args
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| *value)
                .unwrap_or("");
            out.push_str(value);
        } else {
            out.push_str(&rest[start..start + end + 1]);
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}
