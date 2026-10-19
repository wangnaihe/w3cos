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
    ("crypto", include_str!("../js/crypto.js")),
    ("events", include_str!("../js/events.js")),
    ("fs", include_str!("../js/fs.js")),
    ("fs/promises", "module.exports = require('fs').promises;\n"),
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
    ("zlib", include_str!("../js/zlib.js")),
];

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
    fn node_prefix_is_accepted() {
        assert!(source("node:path").is_some());
        assert!(source("node:timers/promises").is_some());
        assert!(source("fs").is_some());
        assert!(source("node:crypto").is_some());
        assert!(source("zlib").is_some());
    }
}
