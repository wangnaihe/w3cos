//! npm package bridge — maps third-party npm imports to w3cos Rust APIs.
//!
//! When the compiler encounters `import { X } from "some-package"`, this module
//! resolves whether the package is a known w3cos-compatible library and returns
//! the corresponding Rust `use` statements and type aliases.
//!
//! # Supported packages
//!
//! | npm package              | w3cos mapping                          |
//! |--------------------------|----------------------------------------|
//! | `@codemirror/view`       | `w3cos_runtime` DOM + observer APIs    |
//! | `@codemirror/state`      | Pure Rust state types (shim)           |
//! | `@codemirror/language`   | Shim (no-op, syntax highlight TBD)     |
//! | `style-mod`              | `w3cos_dom::css_style`                 |
//! | `w3c-keyname`            | Built-in key name mapping              |
//! | `@lezer/*`               | Shim (parser TBD)                      |

use std::collections::HashMap;

/// Resolution result for a single npm import specifier.
#[derive(Debug, Clone)]
pub struct BridgeResolution {
    /// Rust `use` statements to inject at the top of the generated file.
    pub use_stmts: Vec<String>,
    /// Per-symbol type aliases or re-exports to inject.
    pub symbol_aliases: HashMap<String, String>,
    /// Whether this package is fully supported (false = shim/partial).
    pub fully_supported: bool,
    /// Human-readable note about support level.
    pub note: &'static str,
}

impl BridgeResolution {
    fn full(use_stmts: Vec<String>, aliases: &[(&str, &str)]) -> Self {
        Self {
            use_stmts,
            symbol_aliases: aliases.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            fully_supported: true,
            note: "fully supported",
        }
    }

    fn partial(use_stmts: Vec<String>, aliases: &[(&str, &str)], note: &'static str) -> Self {
        Self {
            use_stmts,
            symbol_aliases: aliases.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            fully_supported: false,
            note,
        }
    }

    fn shim(note: &'static str) -> Self {
        Self {
            use_stmts: vec![],
            symbol_aliases: HashMap::new(),
            fully_supported: false,
            note,
        }
    }
}

/// Resolve an npm package specifier to its w3cos bridge.
/// Returns `None` if the package is unknown (will be treated as external).
pub fn resolve_package(specifier: &str) -> Option<BridgeResolution> {
    match specifier {
        // ── CodeMirror view layer ──────────────────────────────────────────
        "@codemirror/view" => Some(BridgeResolution::partial(
            vec![
                "use w3cos_dom::document::Document;".into(),
                "use w3cos_dom::dom_rect::DOMRect;".into(),
                "use w3cos_dom::events::{EventType, InputType, EventData};".into(),
                "use w3cos_runtime::observers::{MutationObserver, MutationObserverInit, MutationRecord, ResizeObserver, IntersectionObserver};".into(),
                "use w3cos_runtime::font_face::FontFaceSet;".into(),
                "use w3cos_runtime::timers::{request_animation_frame, tick};".into(),
                "use w3cos_std::EventAction;".into(),
            ],
            &[],
            "W3C DOM APIs available; EditorView is compiled from the real npm ESM package",
        )),

        // ── CodeMirror state layer ─────────────────────────────────────────
        "@codemirror/state" => Some(BridgeResolution::partial(
            vec![
                "use w3cos_dom::selection::{Range, Selection};".into(),
            ],
            &[],
            "W3C Selection/Range APIs available; EditorState is compiled from the real npm ESM package",
        )),

        // ── CodeMirror language support ────────────────────────────────────
        "@codemirror/language" => Some(BridgeResolution::shim(
            "syntax highlighting shim — tokens parsed but not yet rendered with color",
        )),

        // ── CodeMirror commands ────────────────────────────────────────────
        "@codemirror/commands" => Some(BridgeResolution::shim(
            "keyboard commands shim — key bindings registered via w3cos event system",
        )),

        // ── CodeMirror autocomplete ────────────────────────────────────────
        "@codemirror/autocomplete" => Some(BridgeResolution::shim(
            "autocomplete shim — completion UI TBD",
        )),

        // ── CodeMirror search ──────────────────────────────────────────────
        "@codemirror/search" => Some(BridgeResolution::shim(
            "search/replace shim — panel UI TBD",
        )),

        // ── CodeMirror lint ────────────────────────────────────────────────
        "@codemirror/lint" => Some(BridgeResolution::shim(
            "lint gutter shim — diagnostic display TBD",
        )),

        // ── Lezer parser ──────────────────────────────────────────────────
        s if s.starts_with("@lezer/") => Some(BridgeResolution::shim(
            "Lezer parser shim — incremental parsing TBD",
        )),

        // ── style-mod ─────────────────────────────────────────────────────
        "style-mod" => Some(BridgeResolution::full(
            vec!["use w3cos_dom::css_style::CSSStyleDeclaration;".into()],
            &[],
        )),

        // ── w3c-keyname ───────────────────────────────────────────────────
        "w3c-keyname" => Some(BridgeResolution::full(
            vec![],
            &[],
        )),

        // ── crelt (DOM creation helper used by CodeMirror) ─────────────────
        "crelt" => Some(BridgeResolution::full(
            vec!["use w3cos_dom::document::Document;".into()],
            &[],
        )),

        // Unknown package
        _ => None,
    }
}

/// Analyse all import specifiers in a TypeScript source and return
/// a combined set of Rust `use` statements to inject.
pub fn resolve_imports(specifiers: &[&str]) -> ResolvedImports {
    let mut use_stmts: Vec<String> = Vec::new();
    let mut symbol_map: HashMap<String, String> = HashMap::new();
    let mut unsupported: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for spec in specifiers {
        match resolve_package(spec) {
            Some(res) => {
                for stmt in &res.use_stmts {
                    if !use_stmts.contains(stmt) {
                        use_stmts.push(stmt.clone());
                    }
                }
                symbol_map.extend(res.symbol_aliases);
                if !res.fully_supported {
                    notes.push(format!("{spec}: {}", res.note));
                }
            }
            None => {
                unsupported.push(spec.to_string());
            }
        }
    }

    ResolvedImports { use_stmts, symbol_map, unsupported, notes }
}

/// Result of resolving all imports in a source file.
#[derive(Debug, Default)]
pub struct ResolvedImports {
    /// Rust `use` statements to prepend to the generated file.
    pub use_stmts: Vec<String>,
    /// Map from TS symbol name → Rust path.
    pub symbol_map: HashMap<String, String>,
    /// npm specifiers with no known bridge (will cause compile error if used).
    pub unsupported: Vec<String>,
    /// Human-readable notes about partial support.
    pub notes: Vec<String>,
}

impl ResolvedImports {
    /// Returns true if all imports are fully or partially supported.
    pub fn all_known(&self) -> bool {
        self.unsupported.is_empty()
    }

    /// Format the `use` block as a Rust string.
    pub fn use_block(&self) -> String {
        self.use_stmts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codemirror_view_resolves() {
        let res = resolve_package("@codemirror/view").unwrap();
        assert!(!res.use_stmts.is_empty());
        assert!(res.use_stmts.iter().any(|s| s.contains("w3cos_dom")));
        assert!(res.use_stmts.iter().any(|s| s.contains("w3cos_runtime")));
        println!("[PASS] @codemirror/view resolves to {} use stmts", res.use_stmts.len());
    }

    #[test]
    fn codemirror_state_resolves() {
        let res = resolve_package("@codemirror/state").unwrap();
        assert!(!res.use_stmts.is_empty());
        assert!(res.use_stmts.iter().any(|s| s.contains("selection")));
        println!("[PASS] @codemirror/state resolves");
    }

    #[test]
    fn lezer_shim() {
        let res = resolve_package("@lezer/highlight").unwrap();
        assert!(!res.fully_supported);
        println!("[PASS] @lezer/* returns shim: {}", res.note);
    }

    #[test]
    fn unknown_package_returns_none() {
        assert!(resolve_package("some-random-npm-package").is_none());
        println!("[PASS] unknown package returns None");
    }

    #[test]
    fn resolve_imports_codemirror_bundle() {
        let specs = vec![
            "@codemirror/view",
            "@codemirror/state",
            "@codemirror/language",
            "@codemirror/commands",
            "@lezer/highlight",
            "style-mod",
            "w3c-keyname",
            "crelt",
        ];
        let resolved = resolve_imports(&specs);
        assert!(resolved.unsupported.is_empty(),
            "all CodeMirror deps should be known: {:?}", resolved.unsupported);
        assert!(!resolved.use_stmts.is_empty());
        println!("[PASS] Full CodeMirror import bundle resolved:");
        println!("       {} use stmts, {} symbol mappings",
            resolved.use_stmts.len(), resolved.symbol_map.len());
        for note in &resolved.notes {
            println!("       [shim] {note}");
        }
    }

    #[test]
    fn use_block_format() {
        let resolved = resolve_imports(&["@codemirror/view", "style-mod"]);
        let block = resolved.use_block();
        assert!(block.contains("w3cos_dom"));
        assert!(block.contains("w3cos_runtime"));
        println!("[PASS] use_block:\n{block}");
    }
}
