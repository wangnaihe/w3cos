//! Node.js module resolution pieces shared by the compile-time resolver.
//!
//! Implements the parts of the Node resolution algorithm that depend only on
//! `package.json` contents: `exports` / `imports` resolution with ordered
//! conditions, subpath patterns, fallback arrays and `null` blocks
//! (PACKAGE_EXPORTS_RESOLVE / PACKAGE_IMPORTS_RESOLVE /
//! PACKAGE_TARGET_RESOLVE in the Node ESM spec), the legacy `main` /
//! `module` / `browser` fields, and the builtin module list.
//!
//! Condition order is significant: Node picks the first key of a conditions
//! object that is in the active condition set. `serde_json::Value` (without
//! `preserve_order`) sorts object keys, so `exports` / `imports` are read
//! through [`OrderedJson`], which keeps source order.

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

/// JSON value that preserves object key order.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderedJson {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<OrderedJson>),
    Object(Vec<(String, OrderedJson)>),
}

impl OrderedJson {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            OrderedJson::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&OrderedJson> {
        match self {
            OrderedJson::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for OrderedJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedVisitor;
        impl<'de> Visitor<'de> for OrderedVisitor {
            type Value = OrderedJson;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E: de::Error>(self) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Null)
            }
            fn visit_none<E: de::Error>(self) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Null)
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Bool(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Number(v as f64))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Number(v as f64))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Number(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<OrderedJson, E> {
                Ok(OrderedJson::String(v.to_string()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<OrderedJson, E> {
                Ok(OrderedJson::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<OrderedJson, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(OrderedJson::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedJson, A::Error> {
                let mut entries: Vec<(String, OrderedJson)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, OrderedJson>()? {
                    // Duplicate keys: last one wins (JSON.parse semantics),
                    // keeping the first key's position.
                    if let Some(slot) = entries.iter_mut().find(|(k, _)| *k == key) {
                        slot.1 = value;
                    } else {
                        entries.push((key, value));
                    }
                }
                Ok(OrderedJson::Object(entries))
            }
        }
        deserializer.deserialize_any(OrderedVisitor)
    }
}

/// The fields of `package.json` that take part in resolution.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PackageJson {
    #[serde(default)]
    pub name: Option<OrderedJson>,
    #[serde(default)]
    pub main: Option<OrderedJson>,
    #[serde(default)]
    pub module: Option<OrderedJson>,
    #[serde(default)]
    pub browser: Option<OrderedJson>,
    #[serde(default, rename = "type")]
    pub kind: Option<OrderedJson>,
    #[serde(default)]
    pub exports: Option<OrderedJson>,
    #[serde(default)]
    pub imports: Option<OrderedJson>,
}

impl PackageJson {
    pub fn parse(source: &str) -> serde_json::Result<Self> {
        serde_json::from_str(source)
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().and_then(OrderedJson::as_str)
    }

    pub fn main(&self) -> Option<&str> {
        self.main
            .as_ref()
            .and_then(OrderedJson::as_str)
            .filter(|s| !s.is_empty())
    }

    pub fn module_field(&self) -> Option<&str> {
        self.module
            .as_ref()
            .and_then(OrderedJson::as_str)
            .filter(|s| !s.is_empty())
    }

    pub fn browser_string(&self) -> Option<&str> {
        self.browser
            .as_ref()
            .and_then(OrderedJson::as_str)
            .filter(|s| !s.is_empty())
    }

    /// `"type": "module"` makes `.js` files in the package scope ESM.
    pub fn is_type_module(&self) -> bool {
        self.kind.as_ref().and_then(OrderedJson::as_str) == Some("module")
    }
}

/// Which kind of reference is being resolved. It selects the condition set
/// and, for package roots without `exports`, the legacy entry fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolveKind {
    /// `import` / `export ... from` / `import()`.
    Import,
    /// `require()`.
    Require,
}

impl ResolveKind {
    /// Active `exports` / `imports` conditions. `w3cos` lets a package ship a
    /// w3cos-specific build ahead of everything else; `node` follows because
    /// w3cos provides the Node API surface. `module` and `browser` are kept
    /// for ESM imports (bundler conventions the web pipeline already relied
    /// on). `default` always matches.
    pub fn conditions(self) -> &'static [&'static str] {
        match self {
            ResolveKind::Import => &["w3cos", "node", "import", "module", "browser", "default"],
            ResolveKind::Require => &["w3cos", "node", "require", "default"],
        }
    }
}

/// Outcome of resolving a package target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetResolution {
    /// A target string after pattern substitution (`./dist/x.js`, or a bare
    /// specifier for `imports` targets).
    Found(String),
    /// The subpath is explicitly blocked (`null` target).
    Blocked,
    /// No key / condition matched.
    NoMatch,
}

fn is_conditions_object(entries: &[(String, OrderedJson)]) -> bool {
    !entries.is_empty() && entries.iter().all(|(key, _)| !key.starts_with('.'))
}

/// PACKAGE_EXPORTS_RESOLVE. `subpath` is `"."` or `"./x/y"`.
pub fn resolve_package_exports(
    exports: &OrderedJson,
    subpath: &str,
    conditions: &[&str],
) -> TargetResolution {
    let main_sugar = match exports {
        OrderedJson::String(_) | OrderedJson::Array(_) => true,
        OrderedJson::Object(entries) => is_conditions_object(entries),
        _ => false,
    };
    if main_sugar {
        if subpath == "." {
            return resolve_target(exports, None, false, conditions);
        }
        return TargetResolution::NoMatch;
    }
    let OrderedJson::Object(entries) = exports else {
        return TargetResolution::NoMatch;
    };
    resolve_imports_exports_map(entries, subpath, false, conditions)
}

/// PACKAGE_IMPORTS_RESOLVE. `specifier` starts with `#`.
pub fn resolve_package_imports(
    imports: &OrderedJson,
    specifier: &str,
    conditions: &[&str],
) -> TargetResolution {
    let OrderedJson::Object(entries) = imports else {
        return TargetResolution::NoMatch;
    };
    resolve_imports_exports_map(entries, specifier, true, conditions)
}

/// PACKAGE_IMPORTS_EXPORTS_RESOLVE: exact key, then the best `*` pattern
/// (PATTERN_KEY_COMPARE: longest prefix, then longest key), then legacy
/// trailing-slash folder mappings.
fn resolve_imports_exports_map(
    entries: &[(String, OrderedJson)],
    key: &str,
    is_imports: bool,
    conditions: &[&str],
) -> TargetResolution {
    if !key.contains('*') {
        if let Some((_, target)) = entries.iter().find(|(k, _)| k == key) {
            return resolve_target(target, None, is_imports, conditions);
        }
    }
    let mut best: Option<(&str, &OrderedJson, String)> = None;
    for (pattern, target) in entries {
        let Some(star) = pattern.find('*') else {
            continue;
        };
        if pattern[star + 1..].contains('*') {
            continue;
        }
        let (prefix, suffix) = (&pattern[..star], &pattern[star + 1..]);
        if !key.starts_with(prefix) || key == prefix {
            continue;
        }
        if !suffix.is_empty() && (!key.ends_with(suffix) || key.len() < pattern.len()) {
            continue;
        }
        let matched = key[prefix.len()..key.len() - suffix.len()].to_string();
        let better = match &best {
            None => true,
            Some((current, _, _)) => pattern_key_compare(pattern, current),
        };
        if better {
            best = Some((pattern.as_str(), target, matched));
        }
    }
    if let Some((_, target, matched)) = best {
        return resolve_target(target, Some(&matched), is_imports, conditions);
    }
    // Deprecated folder mappings (`"./sub/": "./lib/sub/"`), still used by
    // older packages.
    let mut folder: Option<(&str, &OrderedJson)> = None;
    for (pattern, target) in entries {
        if pattern.ends_with('/') && key.starts_with(pattern.as_str()) {
            if folder.is_none_or(|(current, _)| pattern.len() > current.len()) {
                folder = Some((pattern.as_str(), target));
            }
        }
    }
    if let Some((pattern, target)) = folder {
        if let OrderedJson::String(base) = target {
            if base.ends_with('/') {
                return TargetResolution::Found(format!("{base}{}", &key[pattern.len()..]));
            }
        }
    }
    TargetResolution::NoMatch
}

/// True when pattern `a` sorts before (is more specific than) `b`.
fn pattern_key_compare(a: &str, b: &str) -> bool {
    let a_base = a.find('*').map_or(a.len(), |i| i + 1);
    let b_base = b.find('*').map_or(b.len(), |i| i + 1);
    if a_base != b_base {
        return a_base > b_base;
    }
    a.len() > b.len()
}

/// PACKAGE_TARGET_RESOLVE.
fn resolve_target(
    target: &OrderedJson,
    pattern_match: Option<&str>,
    is_imports: bool,
    conditions: &[&str],
) -> TargetResolution {
    match target {
        OrderedJson::String(target) => {
            if !target.starts_with("./") {
                // `imports` targets may name another package; `exports`
                // targets must stay inside the package.
                if !is_imports || target.starts_with("../") || target.starts_with('/') {
                    return TargetResolution::NoMatch;
                }
            }
            if target
                .split('/')
                .any(|segment| segment == ".." || segment == "node_modules")
            {
                return TargetResolution::NoMatch;
            }
            match pattern_match {
                Some(matched) => TargetResolution::Found(target.replace('*', matched)),
                None => TargetResolution::Found(target.clone()),
            }
        }
        OrderedJson::Array(items) => {
            let mut last = TargetResolution::NoMatch;
            for item in items {
                match resolve_target(item, pattern_match, is_imports, conditions) {
                    found @ TargetResolution::Found(_) => return found,
                    other => last = other,
                }
            }
            last
        }
        OrderedJson::Object(entries) => {
            for (condition, value) in entries {
                if condition == "default" || conditions.contains(&condition.as_str()) {
                    match resolve_target(value, pattern_match, is_imports, conditions) {
                        TargetResolution::NoMatch => continue,
                        other => return other,
                    }
                }
            }
            TargetResolution::NoMatch
        }
        OrderedJson::Null => TargetResolution::Blocked,
        _ => TargetResolution::NoMatch,
    }
}

const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// Node builtin module (`fs`, `node:fs`, `fs/promises`, `node:test`, ...).
pub fn is_node_builtin(specifier: &str) -> bool {
    if let Some(rest) = specifier.strip_prefix("node:") {
        return !rest.is_empty();
    }
    let head = specifier.split('/').next().unwrap_or_default();
    NODE_BUILTINS.contains(&head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(source: &str) -> OrderedJson {
        serde_json::from_str(source).unwrap()
    }

    const IMPORT: &[&str] = &["w3cos", "node", "import", "module", "browser", "default"];
    const REQUIRE: &[&str] = &["w3cos", "node", "require", "default"];

    fn found(s: &str) -> TargetResolution {
        TargetResolution::Found(s.to_string())
    }

    #[test]
    fn ordered_json_keeps_key_order() {
        let OrderedJson::Object(entries) = json(r#"{"z":1,"a":2,"m":{"y":null,"b":[true]}}"#)
        else {
            panic!()
        };
        let keys: Vec<_> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }

    #[test]
    fn exports_string_and_conditions_sugar() {
        assert_eq!(
            resolve_package_exports(&json(r#""./i.js""#), ".", IMPORT),
            found("./i.js")
        );
        assert_eq!(
            resolve_package_exports(&json(r#""./i.js""#), "./x", IMPORT),
            TargetResolution::NoMatch
        );
        let dual = json(r#"{"import":"./esm.mjs","require":"./cjs.cjs"}"#);
        assert_eq!(
            resolve_package_exports(&dual, ".", IMPORT),
            found("./esm.mjs")
        );
        assert_eq!(
            resolve_package_exports(&dual, ".", REQUIRE),
            found("./cjs.cjs")
        );
    }

    #[test]
    fn condition_key_order_wins_over_condition_priority() {
        // `default` first: always selected, even though `node` is active.
        let e = json(r#"{".":{"default":"./d.js","node":"./n.js"}}"#);
        assert_eq!(resolve_package_exports(&e, ".", REQUIRE), found("./d.js"));
        let e = json(r#"{".":{"browser":"./b.js","node":"./n.js","default":"./d.js"}}"#);
        assert_eq!(resolve_package_exports(&e, ".", REQUIRE), found("./n.js"));
        assert_eq!(resolve_package_exports(&e, ".", IMPORT), found("./b.js"));
        let e = json(r#"{".":{"types":"./t.d.ts","w3cos":"./w.js","default":"./d.js"}}"#);
        assert_eq!(resolve_package_exports(&e, ".", IMPORT), found("./w.js"));
    }

    #[test]
    fn nested_conditions_fall_through() {
        let e = json(
            r#"{".":{"node":{"import":"./node.mjs","require":"./node.cjs"},"default":"./d.js"}}"#,
        );
        assert_eq!(
            resolve_package_exports(&e, ".", IMPORT),
            found("./node.mjs")
        );
        assert_eq!(
            resolve_package_exports(&e, ".", REQUIRE),
            found("./node.cjs")
        );
        let e = json(r#"{".":{"node":{"import":"./node.mjs"},"default":"./d.js"}}"#);
        assert_eq!(resolve_package_exports(&e, ".", REQUIRE), found("./d.js"));
    }

    #[test]
    fn subpath_patterns_null_and_arrays() {
        let e = json(
            r#"{
              "./package.json": "./package.json",
              "./features/*": "./src/features/*.js",
              "./features/*.js": "./src/features/*.js",
              "./features/private/*": null,
              "./lib/*": {"require": "./cjs/*.js", "default": "./esm/*.js"},
              "./fallback": ["./missing-is-not-checked.js", "./other.js"],
              "./old/": "./legacy/"
            }"#,
        );
        assert_eq!(
            resolve_package_exports(&e, "./features/a/b", IMPORT),
            found("./src/features/a/b.js")
        );
        assert_eq!(
            resolve_package_exports(&e, "./features/x.js", IMPORT),
            found("./src/features/x.js")
        );
        assert_eq!(
            resolve_package_exports(&e, "./features/private/s", IMPORT),
            TargetResolution::Blocked
        );
        assert_eq!(
            resolve_package_exports(&e, "./lib/u", REQUIRE),
            found("./cjs/u.js")
        );
        assert_eq!(
            resolve_package_exports(&e, "./lib/u", IMPORT),
            found("./esm/u.js")
        );
        assert_eq!(
            resolve_package_exports(&e, "./fallback", IMPORT),
            found("./missing-is-not-checked.js")
        );
        assert_eq!(
            resolve_package_exports(&e, "./old/a.js", IMPORT),
            found("./legacy/a.js")
        );
        assert_eq!(
            resolve_package_exports(&e, "./nope", IMPORT),
            TargetResolution::NoMatch
        );
        assert_eq!(
            resolve_package_exports(&e, ".", IMPORT),
            TargetResolution::NoMatch
        );
    }

    #[test]
    fn invalid_targets_are_rejected() {
        let e = json(
            r#"{".":"../escape.js","./a":"dep","./b":"./x/../../y.js","./c":"./node_modules/z.js"}"#,
        );
        for sub in [".", "./a", "./b", "./c"] {
            assert_eq!(
                resolve_package_exports(&e, sub, IMPORT),
                TargetResolution::NoMatch,
                "{sub}"
            );
        }
    }

    #[test]
    fn imports_map_allows_bare_targets_and_patterns() {
        let i = json(
            r##"{"#dep":{"node":"dep-node","default":"./polyfill.js"},"#internal/*":"./src/internal/*.js","#blocked":null}"##,
        );
        assert_eq!(
            resolve_package_imports(&i, "#dep", REQUIRE),
            found("dep-node")
        );
        assert_eq!(
            resolve_package_imports(&i, "#internal/a", IMPORT),
            found("./src/internal/a.js")
        );
        assert_eq!(
            resolve_package_imports(&i, "#blocked", IMPORT),
            TargetResolution::Blocked
        );
        assert_eq!(
            resolve_package_imports(&i, "#none", IMPORT),
            TargetResolution::NoMatch
        );
    }

    #[test]
    fn builtins() {
        for s in [
            "fs",
            "node:fs",
            "fs/promises",
            "node:test",
            "path/posix",
            "events",
        ] {
            assert!(is_node_builtin(s), "{s}");
        }
        for s in ["fsx", "semver", "@scope/fs", "node:", "./fs"] {
            assert!(!is_node_builtin(s), "{s}");
        }
    }
}
