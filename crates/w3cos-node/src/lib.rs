//! Node.js builtin modules for W3C OS AOT apps (M2, `w3cos-node`).
//!
//! Each builtin is CommonJS glue in `js/`. The compiler links it into the app
//! like any other CommonJS file, so the glue is AOT-compiled with the app and
//! shares its runtime. Host facts (env, argv, cwd, timers, URL) come from the
//! runtime's globals.

/// (module name, CommonJS source). Subpath modules use their full name.
pub const MODULES: &[(&str, &str)] = &[
    ("assert", include_str!("../js/assert.js")),
    (
        "assert/strict",
        "module.exports = require('assert').strict;\n",
    ),
    ("buffer", include_str!("../js/buffer.js")),
    ("child_process", include_str!("../js/child_process.js")),
    ("crypto", include_str!("../js/crypto.js")),
    ("events", include_str!("../js/events.js")),
    ("fs", include_str!("../js/fs.js")),
    ("fs/promises", "module.exports = require('fs').promises;\n"),
    // Shared poll loop for host-backed handles (child processes, sockets).
    ("internal/w3cos_poll", include_str!("../js/w3cos_poll.js")),
    // Structured clone and worker boot for `worker_threads`.
    ("internal/w3cos_clone", include_str!("../js/w3cos_clone.js")),
    (
        "internal/w3cos_worker",
        include_str!("../js/w3cos_worker.js"),
    ),
    // Native-module replacements (see `NATIVE_REPLACEMENTS`).
    (
        "internal/w3cos_native_keymap",
        include_str!("../js/native_keymap.js"),
    ),
    (
        "internal/w3cos_policy_watcher",
        include_str!("../js/policy_watcher.js"),
    ),
    ("internal/w3cos_spdlog", include_str!("../js/spdlog.js")),
    ("internal/w3cos_sqlite3", include_str!("../js/sqlite3.js")),
    ("internal/w3cos_watcher", include_str!("../js/watcher.js")),
    ("net", include_str!("../js/net.js")),
    ("os", include_str!("../js/os.js")),
    ("path", include_str!("../js/path.js")),
    ("path/posix", "module.exports = require('path');\n"),
    ("process", include_str!("../js/process.js")),
    ("querystring", include_str!("../js/querystring.js")),
    ("stream", include_str!("../js/stream.js")),
    (
        "stream/promises",
        "module.exports = require('stream').promises;\n",
    ),
    ("string_decoder", include_str!("../js/string_decoder.js")),
    ("timers", include_str!("../js/timers.js")),
    ("timers/promises", include_str!("../js/timers_promises.js")),
    ("url", include_str!("../js/url.js")),
    ("util", include_str!("../js/util.js")),
    ("util/types", include_str!("../js/util_types.js")),
    ("worker_threads", include_str!("../js/worker_threads.js")),
    ("zlib", include_str!("../js/zlib.js")),
];

/// `electron` / `@w3cos/electron` (not a Node builtin): Electron's main- and
/// renderer-process API over one AOT-compiled w3cos process. The compiler
/// writes the app's pages and renderer scripts as `./electron_app.js` next
/// to it.
pub const ELECTRON: &str = include_str!("../js/electron.js");

/// npm packages whose native addon w3cos replaces with glue from `MODULES`
/// (M3 item 20). These resolve to the glue even when the package is
/// installed, since its `.node` binary cannot load into an AOT app.
pub const NATIVE_REPLACEMENTS: &[(&str, &str)] = &[
    ("@parcel/watcher", "internal/w3cos_watcher"),
    ("@w3cos/watcher", "internal/w3cos_watcher"),
    ("native-keymap", "internal/w3cos_native_keymap"),
    ("@w3cos/native-keymap", "internal/w3cos_native_keymap"),
    ("@vscode/policy-watcher", "internal/w3cos_policy_watcher"),
    ("@w3cos/policy-watcher", "internal/w3cos_policy_watcher"),
    ("@vscode/spdlog", "internal/w3cos_spdlog"),
    ("@w3cos/spdlog", "internal/w3cos_spdlog"),
    ("@vscode/sqlite3", "internal/w3cos_sqlite3"),
    ("@w3cos/sqlite3", "internal/w3cos_sqlite3"),
];

/// The glue module replacing the native package `specifier`, if any.
pub fn native_replacement(specifier: &str) -> Option<&'static str> {
    NATIVE_REPLACEMENTS
        .iter()
        .find(|(package, _)| *package == specifier)
        .map(|(_, module)| *module)
}

/// The builtin's name without a `node:` prefix.
pub fn module_name(specifier: &str) -> &str {
    specifier.strip_prefix("node:").unwrap_or(specifier)
}

/// CommonJS source for a builtin specifier (`path`, `node:path`, ...).
pub fn source(specifier: &str) -> Option<&'static str> {
    let name = module_name(specifier);
    MODULES
        .iter()
        .find(|(module, _)| *module == name)
        .map(|(_, source)| *source)
}

/// Every `require()` inside the glue names another builtin of this crate.
pub fn is_available(specifier: &str) -> bool {
    source(specifier).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_requires_stay_inside_the_crate() {
        for (name, source) in MODULES {
            let mut rest = *source;
            while let Some(start) = rest.find("require('") {
                rest = &rest[start + "require('".len()..];
                let end = rest.find('\'').unwrap();
                let dep = &rest[..end];
                assert!(is_available(dep), "{name} requires unavailable {dep}");
                rest = &rest[end..];
            }
        }
    }

    #[test]
    fn native_replacements_name_glue_modules() {
        for (package, module) in NATIVE_REPLACEMENTS {
            assert!(is_available(module), "{package} -> missing {module}");
        }
        assert_eq!(
            native_replacement("@vscode/sqlite3"),
            Some("internal/w3cos_sqlite3")
        );
        assert_eq!(native_replacement("sqlite3"), None);
    }

    #[test]
    fn electron_glue_requires_builtins_and_its_app_module() {
        let mut rest = ELECTRON;
        while let Some(start) = rest.find("require('") {
            rest = &rest[start + "require('".len()..];
            let end = rest.find('\'').unwrap();
            let dep = &rest[..end];
            assert!(
                dep == "./electron_app.js" || is_available(dep),
                "electron requires unavailable {dep}"
            );
            rest = &rest[end..];
        }
        assert!(source("electron").is_none() && source("node:electron").is_none());
    }

    #[test]
    fn node_prefix_is_accepted() {
        assert!(source("node:path").is_some());
        assert!(source("node:timers/promises").is_some());
        assert!(source("fs").is_some());
        assert!(source("node:crypto").is_some());
        assert!(source("zlib").is_some());
    }
}
