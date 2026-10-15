//! Fluent-subset locale packs for the shell.
//!
//! Packs live at the repo root (`locales/zh-CN.ftl`, `locales/en.ftl`).
//! Lookup order is the active pack, then zh-CN, then the key itself.
//! A select `{ $count -> [one] ... *[other] ... }` is evaluated with the
//! plural category of the pack that owns the pattern, so a zh-CN fallback
//! is not selected with English categories.
//!
//! `locale-dir` is `ltr` or `rtl` and is applied to the shell root. Both
//! packs are `ltr` today. Plural categories understood now are `one` and
//! `other`. `zero`, `two`, `few`, and `many` can be added for later packs
//! (Arabic, for example) without changing the message syntax.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use w3cos_std::style::TextDirection;

const ZH_SRC: &str = include_str!("../../../locales/zh-CN.ftl");
const EN_SRC: &str = include_str!("../../../locales/en.ftl");

thread_local! {
    static ACTIVE: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn apply_startup() {
    let tag = std::env::var("W3COS_LOCALE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(system_lang)
        .unwrap_or_else(|| "zh-CN".to_string());
    set_locale(&tag);
}

fn system_lang() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed == "C" || trimmed == "POSIX" {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

/// `zh_CN.UTF-8` → `zh-CN`, `en_US.UTF-8` → `en`. Unknown tags become zh-CN.
pub fn resolve_tag(raw: &str) -> String {
    let token = raw
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    let lower = token.to_ascii_lowercase();
    if lower == "zh" || lower.starts_with("zh-") {
        "zh-CN".to_string()
    } else if lower == "en" || lower.starts_with("en-") {
        "en".to_string()
    } else {
        "zh-CN".to_string()
    }
}

pub fn set_locale(tag: &str) {
    let resolved = resolve_tag(tag);
    ACTIVE.with(|slot| *slot.borrow_mut() = resolved);
}

pub fn current() -> String {
    ACTIVE.with(|slot| {
        let value = slot.borrow().clone();
        if value.is_empty() {
            "zh-CN".to_string()
        } else {
            value
        }
    })
}

pub fn text_direction() -> TextDirection {
    if t("locale-dir") == "rtl" {
        TextDirection::Rtl
    } else {
        TextDirection::Ltr
    }
}

/// English uses `one` only for 1. Chinese, and every other pack, uses `other`.
pub fn plural_category(locale: &str, n: i64) -> &'static str {
    if resolve_tag(locale) == "en" && n == 1 {
        "one"
    } else {
        "other"
    }
}

pub fn t(key: &str) -> String {
    t_args(key, &[])
}

pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    resolve_from(packs(), &current(), key, args)
}

pub fn t_in(locale: &str, key: &str) -> String {
    resolve_from(packs(), &resolve_tag(locale), key, &[])
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

fn resolve_from(
    packs: &BTreeMap<String, BTreeMap<String, String>>,
    locale: &str,
    key: &str,
    args: &[(&str, &str)],
) -> String {
    if let Some(pattern) = packs.get(locale).and_then(|pack| pack.get(key)) {
        return eval(locale, pattern, args);
    }
    if locale != "zh-CN"
        && let Some(pattern) = packs.get("zh-CN").and_then(|pack| pack.get(key))
    {
        return eval("zh-CN", pattern, args);
    }
    key.to_string()
}

fn parse_ftl(src: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut key: Option<String> = None;
    let mut lines: Vec<String> = Vec::new();
    let flush =
        |map: &mut BTreeMap<String, String>, key: &mut Option<String>, lines: &mut Vec<String>| {
            if let Some(name) = key.take() {
                let value = lines.join("\n");
                lines.clear();
                if !value.trim().is_empty() {
                    map.insert(name, value);
                }
            }
        };
    for line in src.lines() {
        let trimmed = line.trim().trim_end_matches('\r');
        if key.is_some()
            && !is_message_start(line)
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
        {
            lines.push(trimmed.to_string());
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            flush(&mut map, &mut key, &mut lines);
            continue;
        }
        if let Some((name, value)) = split_message(trimmed) {
            flush(&mut map, &mut key, &mut lines);
            key = Some(name.to_string());
            lines.push(value.to_string());
        }
    }
    flush(&mut map, &mut key, &mut lines);
    map
}

fn is_message_start(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    split_message(line.trim()).is_some()
}

fn split_message(trimmed: &str) -> Option<(&str, &str)> {
    let (name, value) = trimmed.split_once('=')?;
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    Some((name, value.trim()))
}

fn eval(owner: &str, pattern: &str, args: &[(&str, &str)]) -> String {
    let trimmed = pattern.trim();
    if let Some(inner) = select_inner(trimmed) {
        eval_select(owner, inner, args)
    } else {
        substitute(trimmed, args)
    }
}

fn select_inner(pattern: &str) -> Option<&str> {
    if pattern.starts_with('{') && pattern.ends_with('}') && pattern.contains("->") {
        Some(pattern[1..pattern.len() - 1].trim())
    } else {
        None
    }
}

fn eval_select(owner: &str, inner: &str, args: &[(&str, &str)]) -> String {
    let Some((head, body)) = inner.split_once("->") else {
        return substitute(inner, args);
    };
    let name = head.trim().trim_start_matches('$').trim();
    let raw = args
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
        .unwrap_or("0");
    let count: i64 = raw.parse().unwrap_or(0);
    let category = plural_category(owner, count);
    let variants = parse_variants(body);
    let chosen = variants
        .iter()
        .find(|variant| variant.category == category)
        .or_else(|| variants.iter().find(|variant| variant.starred))
        .or_else(|| variants.iter().find(|variant| variant.category == "other"))
        .or_else(|| variants.first());
    match chosen {
        Some(variant) => substitute(&variant.text, args),
        None => substitute(inner, args),
    }
}

struct Variant {
    category: String,
    starred: bool,
    text: String,
}

fn parse_variants(body: &str) -> Vec<Variant> {
    let mut variants = Vec::new();
    let mut rest = body.trim();
    while !rest.is_empty() {
        let starred = rest.starts_with('*');
        if starred {
            rest = rest[1..].trim_start();
        }
        if !rest.starts_with('[') {
            break;
        }
        let Some(end) = rest.find(']') else {
            break;
        };
        let category = rest[1..end].trim().to_string();
        rest = rest[end + 1..].trim_start();
        let next = next_variant_at(rest);
        let text = rest[..next].trim().to_string();
        rest = rest[next..].trim_start();
        variants.push(Variant {
            category,
            starred,
            text,
        });
    }
    variants
}

fn next_variant_at(rest: &str) -> usize {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if offset != 0 && (trimmed.starts_with('[') || trimmed.starts_with("*[")) {
            return offset;
        }
        offset += line.len();
    }
    rest.len()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn every_key_exists_in_all_packs() {
        let packs = packs();
        let zh: BTreeSet<_> = packs["zh-CN"].keys().cloned().collect();
        let en: BTreeSet<_> = packs["en"].keys().cloned().collect();
        assert_eq!(zh, en, "locale packs must have the same keys");
        assert!(zh.contains("locale-dir"));
        assert!(zh.len() > 40);
    }

    #[test]
    fn resolve_tags_and_plurals() {
        assert_eq!(resolve_tag("zh_CN.UTF-8"), "zh-CN");
        assert_eq!(resolve_tag("en_US.UTF-8"), "en");
        assert_eq!(resolve_tag("en"), "en");
        assert_eq!(resolve_tag("fr_FR.UTF-8"), "zh-CN");
        assert_eq!(resolve_tag("C"), "zh-CN");
        assert_eq!(plural_category("en", 1), "one");
        assert_eq!(plural_category("en", 0), "other");
        assert_eq!(plural_category("en", 2), "other");
        assert_eq!(plural_category("zh-CN", 1), "other");
        assert_eq!(plural_category("zh_CN", 5), "other");
    }

    #[test]
    fn lookup_falls_back_to_zh_then_key() {
        set_locale("en");
        assert_eq!(t("not-a-real-key"), "not-a-real-key");
        assert_eq!(t("app-files"), "Files");
        set_locale("zh-CN");
        assert_eq!(t("app-files"), "文件");

        let mut en = BTreeMap::new();
        en.insert(
            "mine".to_string(),
            "{ $count ->\n[one] ONE\n*[other] OTHER\n}".to_string(),
        );
        let mut zh = BTreeMap::new();
        zh.insert(
            "theirs".to_string(),
            "{ $count ->\n[one] ONE\n*[other] OTHER\n}".to_string(),
        );
        let mut packs = BTreeMap::new();
        packs.insert("en".to_string(), en);
        packs.insert("zh-CN".to_string(), zh);
        assert_eq!(resolve_from(&packs, "en", "mine", &[("count", "1")]), "ONE");
        assert_eq!(
            resolve_from(&packs, "en", "theirs", &[("count", "1")]),
            "OTHER",
            "a zh-CN pattern uses Chinese plural categories"
        );
        assert_eq!(resolve_from(&packs, "en", "missing", &[]), "missing");
    }

    #[test]
    fn pack_plurals_and_direction() {
        set_locale("en");
        assert_eq!(t_args("files-count", &[("count", "1")]), "1 item");
        assert_eq!(t_args("files-count", &[("count", "2")]), "2 items");
        assert_eq!(t_args("open-count", &[("count", "1")]), "1 open");
        assert_eq!(text_direction(), TextDirection::Ltr);
        assert_eq!(t_in("zh-CN", "app-files"), "文件");
        set_locale("zh-CN");
        assert_eq!(t_args("files-count", &[("count", "1")]), "1 项");
        assert_eq!(
            t_args("launcher-query", &[("query", "files")]),
            "搜索：files"
        );
        assert_eq!(text_direction(), TextDirection::Ltr);
    }

    #[test]
    fn startup_prefers_w3cos_locale_then_lang() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved = ["W3COS_LOCALE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .map(|key| (key, std::env::var(key).ok()));
        unsafe {
            std::env::set_var("W3COS_LOCALE", "en_US.UTF-8");
            std::env::set_var("LANG", "zh_CN.UTF-8");
        }
        apply_startup();
        assert_eq!(current(), "en");
        unsafe {
            std::env::remove_var("W3COS_LOCALE");
            std::env::set_var("LC_ALL", "C");
            std::env::set_var("LC_MESSAGES", "POSIX");
            std::env::set_var("LANG", "en_GB.UTF-8");
        }
        apply_startup();
        assert_eq!(current(), "en");
        unsafe {
            std::env::remove_var("W3COS_LOCALE");
            std::env::set_var("LC_ALL", "C");
            std::env::set_var("LC_MESSAGES", "POSIX");
            std::env::set_var("LANG", "C.UTF-8");
        }
        apply_startup();
        assert_eq!(current(), "zh-CN");
        for (key, value) in saved {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
