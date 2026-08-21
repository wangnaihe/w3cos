//! Repeatable live-Chromium versus w3cos `window` surface audit.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value as JsonValue, json};
use w3cos_core::Value;

#[derive(Debug, serde::Serialize)]
pub struct AuditReport {
    pub chromium_version: String,
    pub chromium_global_count: usize,
    pub w3cos_global_count: usize,
    pub missing_globals: Vec<String>,
    pub extra_globals: Vec<String>,
    pub missing_global_prototype_members: BTreeMap<String, Vec<String>>,
    pub missing_global_static_members: BTreeMap<String, Vec<String>>,
    pub missing_prototype_members: BTreeMap<String, Vec<String>>,
    pub missing_static_members: BTreeMap<String, Vec<String>>,
}

pub fn run(explicit_chrome: Option<&Path>) -> Result<AuditReport> {
    let chrome = resolve_chrome(explicit_chrome)?;
    let chromium_version = String::from_utf8_lossy(
        &Command::new(&chrome)
            .arg("--version")
            .output()
            .with_context(|| format!("Could not execute {}", chrome.display()))?
            .stdout,
    )
    .trim()
    .to_string();
    let chromium = chromium_inventory(&chrome)?;
    let runtime = runtime_inventory();
    Ok(compare(&chromium_version, &chromium, &runtime))
}

pub fn print_report(report: &AuditReport) {
    println!(
        "Chromium: {} ({} Web API globals)",
        report.chromium_version, report.chromium_global_count
    );
    println!("w3cos: {} exposed globals", report.w3cos_global_count);
    println!(
        "Missing globals ({}): {}",
        report.missing_globals.len(),
        report.missing_globals.join(", ")
    );
    if !report.missing_prototype_members.is_empty() {
        println!("Incomplete prototypes:");
        for (name, members) in &report.missing_prototype_members {
            println!("  {name}: {}", members.join(", "));
        }
    }
    if !report.missing_static_members.is_empty() {
        println!("Incomplete constructor statics:");
        for (name, members) in &report.missing_static_members {
            println!("  {name}: {}", members.join(", "));
        }
    }
    if !report.missing_global_prototype_members.is_empty()
        || !report.missing_global_static_members.is_empty()
    {
        println!(
            "Missing constructor prototypes/statics are available in JSON as \
             missing_global_prototype_members and missing_global_static_members"
        );
    }
    println!(
        "w3cos-only globals ({}): {}",
        report.extra_globals.len(),
        report.extra_globals.join(", ")
    );
}

fn resolve_chrome(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("Chrome executable does not exist: {}", path.display());
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| anyhow!("Chrome/Chromium was not found; pass --chrome <executable>"))
}

fn chromium_inventory(chrome: &Path) -> Result<JsonValue> {
    let nonce = format!(
        "w3cos-web-api-audit-{}-{}.html",
        std::process::id(),
        std::thread::current().name().unwrap_or("main")
    );
    let html_path = std::env::temp_dir().join(nonce);
    let html = r##"<!doctype html><meta charset="utf-8"><pre id="out"></pre><script>
const ignored = new Set([
  "Array","ArrayBuffer","AsyncDisposableStack","Atomics","BigInt","BigInt64Array","BigUint64Array",
  "Boolean","DataView","Date","Error","EvalError","FinalizationRegistry",
  "Float16Array","Float32Array","Float64Array","Function","Infinity","Int8Array",
  "Int16Array","Int32Array","Intl","Iterator","JSON","Map","Math","NaN","Number","Object",
  "Promise","Proxy","RangeError","ReferenceError","Reflect","RegExp","Set",
  "SharedArrayBuffer","String","SuppressedError","Symbol","SyntaxError","Temporal","TypeError","URIError",
  "Uint8Array","Uint8ClampedArray","Uint16Array","Uint32Array","WeakMap",
  "WeakRef","WeakSet","WebAssembly","DisposableStack"
]);
const lowerWebGlobals = new Set([
  "caches","cookieStore","crossOriginIsolated","crypto","customElements",
  "devicePixelRatio","document","history","indexedDB","isSecureContext",
  "getScreenDetails","launchQueue","localStorage","location","navigation","navigator","origin",
  "performance","scheduler","screen","sessionStorage","speechSynthesis",
  "trustedTypes","visualViewport"
]);
const globals = Object.getOwnPropertyNames(window).filter(name =>
  !name.startsWith("on") && !ignored.has(name) &&
  (/^[A-Z]/.test(name) || lowerWebGlobals.has(name))
).sort();
const interfaces = {};
for (const name of globals) {
  let value;
  try { value = window[name]; } catch (_) { continue; }
  if (typeof value === "function" && value.prototype) {
    interfaces[name] = {
      prototype: Object.getOwnPropertyNames(value.prototype).sort(),
      static: Object.getOwnPropertyNames(value).sort()
    };
  }
}
document.querySelector("#out").textContent = JSON.stringify({ globals, interfaces });
</script>"##;
    fs::write(&html_path, html)
        .with_context(|| format!("Could not write {}", html_path.display()))?;
    let output = Command::new(chrome)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-background-networking",
            "--dump-dom",
        ])
        .arg(format!("file://{}", html_path.display()))
        .output()
        .with_context(|| format!("Could not run headless {}", chrome.display()))?;
    let _ = fs::remove_file(&html_path);
    if !output.status.success() {
        bail!(
            "Headless Chromium inventory failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let start_marker = "<pre id=\"out\">";
    let start = stdout
        .find(start_marker)
        .map(|index| index + start_marker.len())
        .ok_or_else(|| anyhow!("Chromium output did not contain the inventory marker"))?;
    let end = stdout[start..]
        .find("</pre>")
        .map(|index| start + index)
        .ok_or_else(|| anyhow!("Chromium inventory marker was not closed"))?;
    let encoded = &stdout[start..end];
    let decoded = encoded
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    serde_json::from_str(&decoded).context("Could not parse Chromium inventory JSON")
}

fn runtime_inventory() -> JsonValue {
    let window = w3cos_runtime::jsdom::window_value();
    let globals: Vec<String> = match &window {
        Value::Object(object) => object
            .borrow()
            .keys()
            .into_iter()
            .filter_map(|name| runtime_public_property_name(&name))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    let mut interfaces = serde_json::Map::new();
    for name in &globals {
        let constructor = window.get_property(name);
        let prototype = constructor.get_property("prototype");
        let members = match prototype {
            Value::Object(object) => {
                let mut keys = object.borrow().keys();
                keys.sort();
                keys
            }
            _ => continue,
        };
        let mut statics = if let Some(function) = constructor.as_function() {
            function.keys()
        } else if let Some(object) = constructor.as_object() {
            object.borrow().keys()
        } else {
            Vec::new()
        };
        statics.sort();
        interfaces.insert(
            name.clone(),
            json!({ "prototype": members, "static": statics }),
        );
    }
    json!({ "globals": globals, "interfaces": interfaces })
}

fn runtime_public_property_name(name: &str) -> Option<String> {
    for prefix in ["__w3cos_getter_", "__w3cos_setter_"] {
        if let Some(public) = name.strip_prefix(prefix) {
            return Some(public.to_string());
        }
    }
    (!name.starts_with("__w3cos_")).then(|| name.to_string())
}

fn string_set(inventory: &JsonValue, key: &str) -> BTreeSet<String> {
    inventory[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

fn compare(version: &str, chromium: &JsonValue, runtime: &JsonValue) -> AuditReport {
    let chromium_globals = string_set(chromium, "globals");
    let runtime_globals = string_set(runtime, "globals");
    let mut missing_prototype_members = BTreeMap::new();
    let mut missing_static_members = BTreeMap::new();
    for name in chromium_globals.intersection(&runtime_globals) {
        let chromium_members = string_set(&chromium["interfaces"][name], "prototype");
        let runtime_members = string_set(&runtime["interfaces"][name], "prototype");
        let missing: Vec<_> = chromium_members
            .difference(&runtime_members)
            .filter(|member| member.as_str() != "constructor")
            .cloned()
            .collect();
        if !missing.is_empty() {
            missing_prototype_members.insert(name.clone(), missing);
        }
        let chromium_statics = string_set(&chromium["interfaces"][name], "static");
        let runtime_statics = string_set(&runtime["interfaces"][name], "static");
        let missing: Vec<_> = chromium_statics
            .difference(&runtime_statics)
            .filter(|member| {
                !matches!(
                    member.as_str(),
                    "arguments" | "caller" | "length" | "name" | "prototype"
                )
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            missing_static_members.insert(name.clone(), missing);
        }
    }
    let missing_globals: Vec<_> = chromium_globals
        .difference(&runtime_globals)
        .cloned()
        .collect();
    let missing_global_prototype_members = missing_globals
        .iter()
        .filter_map(|name| {
            let members: Vec<_> = string_set(&chromium["interfaces"][name], "prototype")
                .into_iter()
                .filter(|member| member != "constructor")
                .collect();
            (!members.is_empty()).then(|| (name.clone(), members))
        })
        .collect();
    let missing_global_static_members = missing_globals
        .iter()
        .filter_map(|name| {
            let members: Vec<_> = string_set(&chromium["interfaces"][name], "static")
                .into_iter()
                .filter(|member| {
                    !matches!(
                        member.as_str(),
                        "arguments" | "caller" | "length" | "name" | "prototype"
                    )
                })
                .collect();
            (!members.is_empty()).then(|| (name.clone(), members))
        })
        .collect();
    AuditReport {
        chromium_version: version.to_string(),
        chromium_global_count: chromium_globals.len(),
        w3cos_global_count: runtime_globals.len(),
        missing_globals,
        extra_globals: runtime_globals
            .difference(&chromium_globals)
            .cloned()
            .collect(),
        missing_global_prototype_members,
        missing_global_static_members,
        missing_prototype_members,
        missing_static_members,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_reports_globals_and_members_deterministically() {
        let chromium = json!({
            "globals": ["Event", "URLPattern"],
            "interfaces": {
                "Event": { "prototype": ["constructor", "type"] },
                "URLPattern": {
                    "prototype": ["constructor", "exec", "test"],
                    "static": ["length", "name", "prototype", "compareComponent"]
                }
            }
        });
        let runtime = json!({
            "globals": ["Event", "Extra"],
            "interfaces": { "Event": { "prototype": ["constructor"] } }
        });
        let report = compare("Test Chrome", &chromium, &runtime);
        assert_eq!(report.missing_globals, vec!["URLPattern"]);
        assert_eq!(
            report.missing_global_prototype_members["URLPattern"],
            vec!["exec", "test"]
        );
        assert_eq!(
            report.missing_global_static_members["URLPattern"],
            vec!["compareComponent"]
        );
        assert_eq!(report.extra_globals, vec!["Extra"]);
        assert_eq!(report.missing_prototype_members["Event"], vec!["type"]);
    }

    #[test]
    fn runtime_inventory_normalizes_internal_accessor_storage() {
        assert_eq!(
            runtime_public_property_name("__w3cos_getter_devicePixelRatio"),
            Some("devicePixelRatio".to_string())
        );
        assert_eq!(
            runtime_public_property_name("__w3cos_setter_document"),
            Some("document".to_string())
        );
        assert_eq!(runtime_public_property_name("__w3cos_private"), None);
        assert_eq!(
            runtime_public_property_name("fetch"),
            Some("fetch".to_string())
        );
    }
}
