//! ESM CSS collection: gathers `.css` files imported by ESM modules and
//! pre-renders them into flat `(selector, declarations)` rules that codegen
//! bakes into the generated bundle's `register_styles()`.
//!
//! Design notes (v1):
//! - Rules keep their FULL selector text (unlike `css_parser`, which drops
//!   combinator chains) because the runtime matcher in `w3cos-dom` evaluates
//!   descendant/child selectors against the live DOM ancestor chain.
//! - Declarations stay raw `(property, value)` strings; unknown properties are
//!   preserved here and dropped by the DOM's `set_property` apply path.
//! - `var(--x)` is resolved at compile time against `:root` / `*` custom
//!   properties collected across ALL collected files (global substitution —
//!   not cascade-correct, documented). Unresolvable `var()` is kept literal.
//! - `calc()` is evaluated only when the whole value is a px-only expression;
//!   anything else (%, rem, var(), `*`/`) is kept literal.
//! - `@media` conditions are retained on each flattened rule for Browser
//!   viewport evaluation. Build-time AOT registration still includes those
//!   rules unconditionally until native viewport-conditioned registration is
//!   introduced. `@supports` / `@layer` blocks are included, while
//!   `@keyframes` are skipped.
//! - Leading `@import` URLs and media conditions are exposed as ordered
//!   dependency metadata; fetching remains owned by the Browser loader.
//! - `@font-face` families, ordered `local()`/`url()` sources, formats and
//!   descriptors are exposed as metadata for the same Browser loader.
//! - Every problem degrades to a warning string; nothing here fails a build.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::css_values;
use crate::esm_resolver::{EsmResolver, ModuleGraph, is_asset_import};

/// A single flat rule ready for `register_rule` codegen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedRule {
    /// Single (non-comma) selector, full text including combinators.
    pub selector: String,
    /// Raw declarations in source order (custom properties excluded).
    pub declarations: Vec<(String, String)>,
    /// Combined enclosing `@media` condition, if any.
    pub media: Option<String>,
}

/// One leading CSS `@import` dependency discovered by the shared tolerant
/// parser. URL resolution and fetching remain the caller's responsibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StylesheetImport {
    pub href: String,
    pub media: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StylesheetFontSource {
    Url {
        href: String,
        format: Option<String>,
    },
    Local(String),
}

/// One `@font-face` declaration parsed from authored CSS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StylesheetFontFace {
    pub family: String,
    pub sources: Vec<StylesheetFontSource>,
    pub weight: Option<String>,
    pub style: Option<String>,
    pub display: Option<String>,
    pub unicode_range: Option<String>,
    pub media: Option<String>,
}

/// Result of collecting CSS imports from a module graph.
#[derive(Debug, Clone, Default)]
pub struct CollectedStylesheet {
    /// Number of distinct `.css` files read.
    pub files: usize,
    /// Flat rules in (file, source) order — the cascade's registration order.
    pub rules: Vec<CollectedRule>,
    /// Leading `@import` dependencies in authored order.
    pub imports: Vec<StylesheetImport>,
    /// Parsed `@font-face` declarations in authored order.
    pub font_faces: Vec<StylesheetFontFace>,
    /// Human-readable warnings (bad css, skipped preprocessors, unresolved
    /// vars). Surfaced as `//! WARNING css ...` diagnostics in the bundle.
    pub warnings: Vec<String>,
}

impl CollectedStylesheet {
    fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if !self.warnings.contains(&msg) && self.warnings.len() < 200 {
            self.warnings.push(msg);
        }
    }
}

/// Collect every `.css` asset import retained in the module graph.
///
/// Graph nodes keep ALL import specifiers (asset imports are only skipped for
/// JS recursion), so this is a pure walk: filter → resolve → read → parse.
/// `.scss` / `.sass` / `.less` imports are skipped with a warning.
pub fn collect_esm_css(graph: &ModuleGraph, resolver: &EsmResolver) -> CollectedStylesheet {
    let mut out = CollectedStylesheet::default();
    let mut seen_files: HashSet<std::path::PathBuf> = HashSet::new();
    let mut raw_rules: Vec<RawRule> = Vec::new();
    let mut custom_props: HashMap<String, String> = HashMap::new();

    for node in &graph.nodes {
        let from_dir = node.module.path.parent().unwrap_or_else(|| Path::new("."));
        for specifier in &node.imports {
            if !is_asset_import(specifier) {
                continue;
            }
            let ext = Path::new(specifier)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "css" => {}
                "scss" | "sass" | "less" => {
                    out.warn(format!(
                        "css {specifier}: preprocessor stylesheets are not supported in the ESM pipeline, skipped"
                    ));
                    continue;
                }
                // json/wasm/images etc. — not stylesheet business.
                _ => continue,
            }

            let resolved = match resolver.resolve(specifier, from_dir) {
                Ok(resolved) => resolved,
                Err(err) => {
                    out.warn(format!("css {specifier}: could not resolve: {err}"));
                    continue;
                }
            };
            if !seen_files.insert(resolved.path.clone()) {
                continue; // dedupe: same file imported from several modules
            }
            let source = match std::fs::read_to_string(&resolved.path) {
                Ok(source) => source,
                Err(err) => {
                    out.warn(format!(
                        "css {}: could not read: {err}",
                        resolved.path.display()
                    ));
                    continue;
                }
            };
            out.files += 1;
            let display = resolved.path.display().to_string();
            let file_rules = parse_css_raw(
                &source,
                &display,
                &mut out.warnings,
                &mut out.imports,
                &mut out.font_faces,
            );
            ingest_raw_rules(file_rules, &mut raw_rules, &mut custom_props);
        }
    }

    finish_raw_rules(&raw_rules, &custom_props, &mut out);
    out
}

/// Parse one authored stylesheet through the same tolerant parser and
/// normalization path used by build-time ESM CSS imports. Dynamic Browser
/// targets consume this API from the capability-scoped loader, so Browser and
/// native AOT do not grow separate CSS parsers.
pub fn parse_css_source(source: &str, source_url: &str) -> CollectedStylesheet {
    let mut out = CollectedStylesheet {
        files: 1,
        ..CollectedStylesheet::default()
    };
    let mut raw_rules = Vec::new();
    let mut custom_props = HashMap::new();
    let parsed = parse_css_raw(
        source,
        source_url,
        &mut out.warnings,
        &mut out.imports,
        &mut out.font_faces,
    );
    ingest_raw_rules(parsed, &mut raw_rules, &mut custom_props);
    finish_raw_rules(&raw_rules, &custom_props, &mut out);
    out
}

fn ingest_raw_rules(
    rules: Vec<RawRule>,
    raw_rules: &mut Vec<RawRule>,
    custom_props: &mut HashMap<String, String>,
) {
    for rule in rules {
        // Custom properties are collected from :root / * rules only and used
        // for var() substitution. They are not emitted as declarations.
        let trimmed = rule.selectors.trim();
        if trimmed == ":root" || trimmed == "*" || trimmed == "html" {
            for (prop, value) in &rule.declarations {
                if prop.starts_with("--") {
                    custom_props.insert(prop.clone(), value.clone());
                }
            }
        }
        raw_rules.push(rule);
    }
}

fn finish_raw_rules(
    raw_rules: &[RawRule],
    custom_props: &HashMap<String, String>,
    out: &mut CollectedStylesheet,
) {
    // Flatten: split comma groups and resolve globally-known var()/calc().
    // Scoped custom properties must remain in the emitted rule so the native
    // DOM can apply normal cascade and inheritance semantics at runtime.
    let mut unresolved_vars: Vec<String> = Vec::new();
    let mut literal_calcs: Vec<String> = Vec::new();
    for rule in raw_rules {
        let declarations: Vec<(String, String)> = rule
            .declarations
            .iter()
            .map(|(prop, value)| {
                if prop.starts_with("--") {
                    return (prop.clone(), value.clone());
                }
                let value = finalize_value(
                    value,
                    &custom_props,
                    &mut unresolved_vars,
                    &mut literal_calcs,
                );
                (prop.clone(), value)
            })
            .collect();
        if declarations.is_empty() {
            continue;
        }
        for selector in split_selector_group(&rule.selectors) {
            out.rules.push(CollectedRule {
                selector,
                declarations: declarations.clone(),
                media: rule.media.clone(),
            });
        }
    }

    for name in unresolved_vars.iter().take(50) {
        out.warn(format!("css: unresolved var({name}) kept literal"));
    }
    if unresolved_vars.len() > 50 {
        out.warn(format!(
            "css: +{} more unresolved var() names",
            unresolved_vars.len() - 50
        ));
    }
    for value in literal_calcs.iter().take(20) {
        out.warn(format!("css: non-px calc() kept literal: {value}"));
    }
}

/// A raw parsed rule: unsplit selector group + unprocessed declarations.
#[derive(Debug)]
struct RawRule {
    selectors: String,
    declarations: Vec<(String, String)>,
    media: Option<String>,
}

/// Tolerant raw CSS rule extraction. Never fails: malformed input produces
/// warnings and best-effort rules.
fn parse_css_raw(
    source: &str,
    path: &str,
    warnings: &mut Vec<String>,
    imports: &mut Vec<StylesheetImport>,
    font_faces: &mut Vec<StylesheetFontFace>,
) -> Vec<RawRule> {
    let source = strip_block_comments(source);
    let open = source.matches('{').count();
    let close = source.matches('}').count();
    if open != close {
        push_warning(
            warnings,
            format!(
                "css {path}: unbalanced braces ({open} '{{' vs {close} '}}'), parsed best-effort"
            ),
        );
    }
    let mut rules = Vec::new();
    parse_block_into(
        &source, path, warnings, &mut rules, imports, font_faces, None, true,
    );
    if rules.is_empty() && source.trim().len() > 16 {
        push_warning(
            warnings,
            format!("css {path}: no rules parsed from non-empty file"),
        );
    }
    rules
}

fn push_warning(warnings: &mut Vec<String>, msg: String) {
    if !warnings.contains(&msg) && warnings.len() < 200 {
        warnings.push(msg);
    }
}

/// Parse a block of CSS (top level or inside @media/@supports/@layer).
fn parse_block_into(
    source: &str,
    path: &str,
    warnings: &mut Vec<String>,
    rules: &mut Vec<RawRule>,
    imports: &mut Vec<StylesheetImport>,
    font_faces: &mut Vec<StylesheetFontFace>,
    media: Option<&str>,
    top_level: bool,
) {
    let bytes = source.as_bytes();
    let mut pos = 0;
    let mut imports_allowed = top_level;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        if bytes[pos] == b'@' {
            let (next, keeps_imports_open) = parse_at_rule(
                &source,
                pos,
                path,
                warnings,
                rules,
                imports,
                font_faces,
                media,
                imports_allowed,
            );
            pos = next;
            imports_allowed &= keeps_imports_open;
            continue;
        }

        // Normal rule: selectors { declarations }
        let selector_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos >= bytes.len() {
            let tail = source[selector_start..].trim();
            if !tail.is_empty() {
                push_warning(
                    warnings,
                    format!("css {path}: truncated rule near `{}`", truncate(&tail, 40)),
                );
            }
            break;
        }
        let selector_str = source[selector_start..pos].trim();
        pos += 1;

        let (block_str, advance, terminated) = extract_brace_content(&source[pos..]);
        pos += advance;
        if !terminated {
            push_warning(
                warnings,
                format!(
                    "css {path}: unterminated block for selector `{}`",
                    truncate(selector_str, 40)
                ),
            );
        }
        if !selector_str.is_empty() {
            imports_allowed = false;
            let declarations = parse_declarations_raw(block_str);
            rules.push(RawRule {
                selectors: selector_str.to_string(),
                declarations,
                media: media.map(ToString::to_string),
            });
        }
        if !terminated {
            break;
        }
    }
}

fn parse_at_rule(
    source: &str,
    start: usize,
    path: &str,
    warnings: &mut Vec<String>,
    rules: &mut Vec<RawRule>,
    imports: &mut Vec<StylesheetImport>,
    font_faces: &mut Vec<StylesheetFontFace>,
    media: Option<&str>,
    imports_allowed: bool,
) -> (usize, bool) {
    let bytes = source.as_bytes();
    let mut pos = start + 1;

    let kw_start = pos;
    while pos < bytes.len() && (bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'-') {
        pos += 1;
    }
    let keyword = &source[kw_start..pos];

    // Find the end of the at-rule prelude: first ';' or '{'.
    let mut scan = pos;
    while scan < bytes.len() && bytes[scan] != b';' && bytes[scan] != b'{' {
        scan += 1;
    }
    if scan >= bytes.len() {
        return (scan, false);
    }
    let prelude = source[pos..scan].trim().to_string();

    if bytes[scan] == b';' {
        if keyword.eq_ignore_ascii_case("import") && imports_allowed {
            if let Some(import) = parse_import_prelude(&prelude) {
                imports.push(import);
            } else {
                push_warning(
                    warnings,
                    format!("css {path}: malformed @import skipped: {prelude}"),
                );
            }
        }
        return (
            scan + 1,
            keyword.eq_ignore_ascii_case("import") || keyword.eq_ignore_ascii_case("charset"),
        );
    }

    // At-rule with a block.
    pos = scan + 1;
    let (block_str, advance, _terminated) = extract_brace_content(&source[pos..]);
    pos += advance;
    match keyword {
        "media" => {
            let combined = media
                .map(|parent| format!("({parent}) and ({prelude})"))
                .unwrap_or(prelude);
            parse_block_into(
                block_str,
                path,
                warnings,
                rules,
                imports,
                font_faces,
                Some(&combined),
                false,
            );
        }
        // Other grouping blocks retain any enclosing media condition.
        "supports" | "layer" => {
            parse_block_into(
                block_str, path, warnings, rules, imports, font_faces, media, false,
            );
        }
        "font-face" => {
            if let Some(face) = parse_font_face_block(block_str, media) {
                font_faces.push(face);
            } else {
                push_warning(
                    warnings,
                    format!("css {path}: @font-face missing family or usable src"),
                );
            }
        }
        // keyframes: no style rules for the registry.
        _ => {}
    }
    (pos, false)
}

fn parse_import_prelude(prelude: &str) -> Option<StylesheetImport> {
    let prelude = prelude.trim();
    let (href, rest) = if let Some(quoted) = prelude.strip_prefix('"') {
        let end = quoted.find('"')?;
        (quoted[..end].to_string(), &quoted[end + 1..])
    } else if let Some(quoted) = prelude.strip_prefix('\'') {
        let end = quoted.find('\'')?;
        (quoted[..end].to_string(), &quoted[end + 1..])
    } else {
        let after_url = prelude
            .get(..4)
            .filter(|prefix| prefix.eq_ignore_ascii_case("url("))
            .map(|_| &prelude[4..])?;
        let end = after_url.find(')')?;
        let href = after_url[..end]
            .trim()
            .trim_matches(|character| character == '"' || character == '\'')
            .to_string();
        (href, &after_url[end + 1..])
    };
    if href.is_empty() {
        return None;
    }
    let media = rest.trim();
    Some(StylesheetImport {
        href,
        media: (!media.is_empty()).then(|| media.to_string()),
    })
}

fn parse_font_face_block(block: &str, media: Option<&str>) -> Option<StylesheetFontFace> {
    let declarations = parse_declarations_raw(block);
    let value = |name: &str| {
        declarations
            .iter()
            .find(|(property, _)| property.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let family = value("font-family")?
        .trim()
        .trim_matches(|character| character == '"' || character == '\'')
        .to_string();
    if family.is_empty() {
        return None;
    }
    let sources = split_top_level(value("src")?, b',')
        .into_iter()
        .filter_map(|candidate| {
            let candidate = candidate.trim();
            if let Some((name, _)) = parse_css_function(candidate, "local") {
                return Some(StylesheetFontSource::Local(name));
            }
            let (href, rest) = parse_css_function(candidate, "url")?;
            if href.is_empty() {
                return None;
            }
            let format = parse_css_function(rest.trim(), "format").map(|(format, _)| format);
            Some(StylesheetFontSource::Url { href, format })
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return None;
    }
    Some(StylesheetFontFace {
        family,
        sources,
        weight: value("font-weight").map(ToString::to_string),
        style: value("font-style").map(ToString::to_string),
        display: value("font-display").map(ToString::to_string),
        unicode_range: value("unicode-range").map(ToString::to_string),
        media: media.map(ToString::to_string),
    })
}

fn parse_css_function<'a>(source: &'a str, name: &str) -> Option<(String, &'a str)> {
    let source = source.trim_start();
    let open = source.find('(')?;
    if !source[..open].trim().eq_ignore_ascii_case(name) {
        return None;
    }
    let mut quote = None;
    let mut depth = 0_i32;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '"' | '\'' if quote == Some(character) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(character),
            '(' if quote.is_none() => depth += 1,
            ')' if quote.is_none() => {
                depth -= 1;
                if depth == 0 {
                    let end = open + offset;
                    let value = source[open + 1..end]
                        .trim()
                        .trim_matches(|character| character == '"' || character == '\'')
                        .to_string();
                    return Some((value, &source[end + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract content between a `{` (already consumed) and its matching `}`.
/// Returns (content, bytes_consumed, terminated).
fn extract_brace_content(s: &str) -> (&str, usize, bool) {
    let bytes = s.as_bytes();
    let mut depth = 1i32;
    let mut pos = 0;
    while pos < bytes.len() && depth > 0 {
        if bytes[pos] == b'{' {
            depth += 1;
        }
        if bytes[pos] == b'}' {
            depth -= 1;
        }
        if depth > 0 {
            pos += 1;
        }
    }
    let terminated = pos < bytes.len();
    let content = &s[..pos];
    let consumed = if terminated { pos + 1 } else { pos };
    (content, consumed, terminated)
}

/// Strip `/* ... */` comments. (`//` is NOT a CSS comment — stripping it
/// would corrupt `url(...)` values.)
fn strip_block_comments(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// Split a declaration block on top-level `;` (paren-aware, so `url(...;...)`
/// and `var(--x, a; b)` values survive), then on the first top-level `:`.
fn parse_declarations_raw(block: &str) -> Vec<(String, String)> {
    let mut declarations = Vec::new();
    for segment in split_top_level(block, b';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let mut depth = 0i32;
        let mut colon = None;
        for (i, ch) in segment.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ':' if depth == 0 => {
                    colon = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(colon) = colon else {
            continue; // not a `prop: value` pair — tolerated, skipped
        };
        let prop = segment[..colon].trim();
        let value = segment[colon + 1..].trim();
        // `!important` is parsed but treated as a normal declaration in v1.
        let value = value.trim_end_matches("!important").trim();
        if !prop.is_empty() && !value.is_empty() {
            declarations.push((prop.to_string(), value.to_string()));
        }
    }
    declarations
}

fn split_top_level(s: &str, sep: u8) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if ch == sep as char && depth == 0 {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    parts
}

/// Split a selector group on top-level commas (paren/bracket aware).
fn split_selector_group(selectors: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    for ch in selectors.chars() {
        match ch {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            ',' if paren == 0 && bracket == 0 => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts
}

/// Compile-time value finalization: global var() substitution + px-only calc.
fn finalize_value(
    value: &str,
    custom_props: &HashMap<String, String>,
    unresolved_vars: &mut Vec<String>,
    literal_calcs: &mut Vec<String>,
) -> String {
    let resolved = if value.contains("var(") {
        resolve_vars(value, custom_props, unresolved_vars)
    } else {
        value.to_string()
    };

    let trimmed = resolved.trim();
    if trimmed.starts_with("calc(") && trimmed.ends_with(')') {
        if let Some(px) = css_values::css_parse_calc_px(trimmed) {
            return format!("{px}px");
        }
        if !literal_calcs.contains(&trimmed.to_string()) {
            literal_calcs.push(trimmed.to_string());
        }
    }
    resolved
}

/// Iteratively substitute `var(--x)` / `var(--x, fallback)` from the global
/// custom-property map. Unresolvable references are KEPT LITERAL (the runtime
/// theme system may provide them) and their names recorded once each.
fn resolve_vars(
    value: &str,
    custom_props: &HashMap<String, String>,
    unresolved_vars: &mut Vec<String>,
) -> String {
    let mut current = value.to_string();
    // Bounded passes: a substituted value may itself contain var().
    for _ in 0..10 {
        let (next, changed) = resolve_vars_pass(&current, custom_props, unresolved_vars);
        current = next;
        if !changed {
            break;
        }
    }
    current
}

/// One substitution pass. Returns the rewritten string and whether any
/// reference was substituted.
fn resolve_vars_pass(
    value: &str,
    custom_props: &HashMap<String, String>,
    unresolved_vars: &mut Vec<String>,
) -> (String, bool) {
    let mut result = String::with_capacity(value.len());
    let mut changed = false;
    let mut rest = value;
    while let Some(start) = rest.find("var(") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let mut depth = 1i32;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            result.push_str(&rest[start..]); // unbalanced — keep literal
            return (result, changed);
        };
        let inner = after[..end].trim();
        let (var_name, fallback) = match inner.find(',') {
            Some(comma) => (inner[..comma].trim(), Some(inner[comma + 1..].trim())),
            None => (inner, None),
        };
        if let Some(v) = custom_props.get(var_name) {
            result.push_str(v);
            changed = true;
        } else if let Some(f) = fallback {
            result.push_str(f);
            changed = true;
        } else {
            if !unresolved_vars.contains(&var_name.to_string()) {
                unresolved_vars.push(var_name.to_string());
            }
            result.push_str(&rest[start..start + "var(".len() + end + 1]); // keep literal
        }
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    (result, changed)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm_resolver::EsmResolver;

    fn write_fixture(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "w3cos_esm_css_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        for (name, content) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        root
    }

    fn collect(root: &std::path::Path, entry: &str) -> CollectedStylesheet {
        let resolver = EsmResolver::new(root);
        let graph = resolver
            .build_graph_from_entry(&root.join(entry))
            .expect("graph build");
        collect_esm_css(&graph, &resolver)
    }

    #[test]
    fn runtime_source_uses_the_same_css_normalization_pipeline() {
        let sheet = parse_css_source(
            ":root { --accent: #123456; }\n\
             .panel, .card { color: var(--accent); width: calc(4px + 6px); }",
            "https://example.test/app.css",
        );
        assert_eq!(sheet.files, 1);
        assert!(sheet.warnings.is_empty(), "warnings: {:?}", sheet.warnings);
        assert_eq!(
            sheet
                .rules
                .iter()
                .map(|rule| rule.selector.as_str())
                .collect::<Vec<_>>(),
            [":root", ".panel", ".card"]
        );
        assert_eq!(
            sheet.rules[0].declarations,
            [("--accent".to_string(), "#123456".to_string())]
        );
        for rule in &sheet.rules[1..] {
            assert_eq!(
                rule.declarations,
                [
                    ("color".to_string(), "#123456".to_string()),
                    ("width".to_string(), "10px".to_string())
                ]
            );
        }
    }

    #[test]
    fn collects_css_from_two_module_graph() {
        let root = write_fixture(
            "two_module",
            &[
                (
                    "src/app.ts",
                    "import './mod';\nimport './a.css';\nexport function main() {}",
                ),
                (
                    "src/mod.ts",
                    "import './b.css';\nimport './a.css';\nexport const x = 1;",
                ),
                ("src/a.css", ".alpha { color: red; }"),
                ("src/b.css", ".beta { width: 10px; }"),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        assert_eq!(sheet.files, 2, "a.css must be deduped: {sheet:?}");
        let selectors: Vec<&str> = sheet.rules.iter().map(|r| r.selector.as_str()).collect();
        assert!(selectors.contains(&".alpha"));
        assert!(selectors.contains(&".beta"));
        assert!(sheet.warnings.is_empty(), "warnings: {:?}", sheet.warnings);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bad_css_warns_and_continues() {
        let root = write_fixture(
            "bad_css",
            &[
                (
                    "src/app.ts",
                    "import './bad.css';\nimport './good.css';\nexport function main() {}",
                ),
                ("src/bad.css", ".broken { color: red;"),
                ("src/good.css", ".fine { gap: 4px; }"),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        assert!(
            sheet.warnings.iter().any(|w| w.contains("bad.css")),
            "expected bad.css warning: {:?}",
            sheet.warnings
        );
        assert!(
            sheet.rules.iter().any(|r| r.selector == ".fine"),
            "good.css rules must still be collected: {sheet:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scss_import_warns_and_skips() {
        let root = write_fixture(
            "scss",
            &[
                (
                    "src/app.ts",
                    "import './s.scss';\nexport function main() {}",
                ),
                ("src/s.scss", ".x { color: red; }"),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        assert_eq!(sheet.files, 0);
        assert!(sheet.rules.is_empty());
        assert!(
            sheet.warnings.iter().any(|w| w.contains("s.scss")),
            "warnings: {:?}",
            sheet.warnings
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn var_substitution_against_root_custom_props() {
        let root = write_fixture(
            "var",
            &[
                ("src/app.ts", "import './v.css';\nexport function main() {}"),
                (
                    "src/v.css",
                    ":root { --pad: 8px; --mono: monospace; }\n\
                 .a { width: var(--pad); font-family: var(--mono); \
                 top: var(--missing, 9px); left: var(--gone); }",
                ),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        let rule = sheet.rules.iter().find(|r| r.selector == ".a").unwrap();
        let get = |prop: &str| {
            rule.declarations
                .iter()
                .find(|(p, _)| p == prop)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("width"), Some("8px"));
        assert_eq!(get("font-family"), Some("monospace"));
        assert_eq!(get("top"), Some("9px"), "fallback must be used");
        assert_eq!(
            get("left"),
            Some("var(--gone)"),
            "unresolved var must stay literal"
        );
        assert!(
            sheet.warnings.iter().any(|w| w.contains("--gone")),
            "warnings: {:?}",
            sheet.warnings
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn calc_px_evaluated_non_px_kept_literal() {
        let root = write_fixture(
            "calc",
            &[
                ("src/app.ts", "import './c.css';\nexport function main() {}"),
                (
                    "src/c.css",
                    ".b { width: calc(10px + 4px); left: calc(50% - 10px); top: calc(20 * 22px); }",
                ),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        let rule = sheet.rules.iter().find(|r| r.selector == ".b").unwrap();
        let get = |prop: &str| {
            rule.declarations
                .iter()
                .find(|(p, _)| p == prop)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("width"), Some("14px"));
        assert_eq!(get("left"), Some("calc(50% - 10px)"));
        assert_eq!(get("top"), Some("calc(20 * 22px)"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn comma_groups_split_and_media_condition_is_retained() {
        let root = write_fixture(
            "comma",
            &[
                ("src/app.ts", "import './m.css';\nexport function main() {}"),
                (
                    "src/m.css",
                    ".a, .b { color: red; }\n\
                 @media (max-width: 600px) { .c { color: blue; } }\n\
                 @keyframes spin { from { opacity: 0; } to { opacity: 1; } }",
                ),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        let selectors: Vec<&str> = sheet.rules.iter().map(|r| r.selector.as_str()).collect();
        assert_eq!(
            selectors,
            vec![".a", ".b", ".c"],
            "comma group splits, @media included, @keyframes skipped: {selectors:?}"
        );
        assert_eq!(
            sheet
                .rules
                .iter()
                .find(|rule| rule.selector == ".c")
                .and_then(|rule| rule.media.as_deref()),
            Some("(max-width: 600px)")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nested_media_conditions_are_combined_on_flattened_rules() {
        let sheet = parse_css_source(
            "@media screen and (min-width: 400px) {\
                @supports (display: grid) { .grid { display: grid; } }\
                @media (max-width: 900px) { .compact { gap: 4px; } }\
            }",
            "nested.css",
        );
        assert_eq!(
            sheet
                .rules
                .iter()
                .find(|rule| rule.selector == ".grid")
                .and_then(|rule| rule.media.as_deref()),
            Some("screen and (min-width: 400px)")
        );
        assert_eq!(
            sheet
                .rules
                .iter()
                .find(|rule| rule.selector == ".compact")
                .and_then(|rule| rule.media.as_deref()),
            Some("(screen and (min-width: 400px)) and ((max-width: 900px))")
        );
    }

    #[test]
    fn leading_imports_preserve_authored_order_urls_and_media() {
        let sheet = parse_css_source(
            "@charset \"utf-8\";\
             @import \"reset.css\";\
             @import url('./theme.css') screen and (min-width: 600px);\
             .root { color: black; }\
             @import \"too-late.css\";",
            "imports.css",
        );
        assert_eq!(
            sheet.imports,
            vec![
                StylesheetImport {
                    href: "reset.css".to_string(),
                    media: None,
                },
                StylesheetImport {
                    href: "./theme.css".to_string(),
                    media: Some("screen and (min-width: 600px)".to_string()),
                },
            ]
        );
    }

    #[test]
    fn font_faces_preserve_order_sources_descriptors_and_media() {
        let sheet = parse_css_source(
            "@font-face {\
               font-family: 'Map Sans';\
               src: local('Map Sans'), url('../fonts/map.woff2') format('woff2');\
               font-weight: 400;\
               font-style: italic;\
               font-display: swap;\
               unicode-range: U+0000-00FF;\
             }\
             @media screen and (min-width: 800px) {\
               @font-face { font-family: Wide; src: url(wide.ttf) format(truetype); }\
             }",
            "fonts.css",
        );
        assert_eq!(sheet.font_faces.len(), 2);
        assert_eq!(sheet.font_faces[0].family, "Map Sans");
        assert_eq!(
            sheet.font_faces[0].sources,
            vec![
                StylesheetFontSource::Local("Map Sans".to_string()),
                StylesheetFontSource::Url {
                    href: "../fonts/map.woff2".to_string(),
                    format: Some("woff2".to_string()),
                },
            ]
        );
        assert_eq!(sheet.font_faces[0].weight.as_deref(), Some("400"));
        assert_eq!(sheet.font_faces[0].style.as_deref(), Some("italic"));
        assert_eq!(sheet.font_faces[0].display.as_deref(), Some("swap"));
        assert_eq!(
            sheet.font_faces[0].unicode_range.as_deref(),
            Some("U+0000-00FF")
        );
        assert_eq!(
            sheet.font_faces[1].media.as_deref(),
            Some("screen and (min-width: 800px)")
        );
    }

    #[test]
    fn scoped_custom_properties_are_emitted_for_runtime_cascade() {
        let root = write_fixture(
            "custom_props",
            &[
                ("src/app.ts", "import './p.css';\nexport function main() {}"),
                (
                    "src/p.css",
                    ":root { --x: 1px; }\n.themed { --y: 2px; color: red; }",
                ),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        let themed = sheet
            .rules
            .iter()
            .find(|rule| rule.selector == ".themed")
            .unwrap();
        assert!(
            themed
                .declarations
                .contains(&("--y".to_string(), "2px".to_string()))
        );
        let root_rule = sheet
            .rules
            .iter()
            .find(|rule| rule.selector == ":root")
            .unwrap();
        assert!(
            root_rule
                .declarations
                .contains(&("--x".to_string(), "1px".to_string()))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn descendant_selector_text_preserved() {
        let root = write_fixture(
            "descendant",
            &[
                ("src/app.ts", "import './d.css';\nexport function main() {}"),
                (
                    "src/d.css",
                    ".monaco-editor .find-widget { position: absolute; }\n\
                 .hc-black .monaco-select-box-dropdown-padding, .hc-light .monaco-select-box-dropdown-padding { padding: 3px; }",
                ),
            ],
        );
        let sheet = collect(&root, "src/app.ts");
        let selectors: Vec<&str> = sheet.rules.iter().map(|r| r.selector.as_str()).collect();
        assert!(selectors.contains(&".monaco-editor .find-widget"));
        assert!(selectors.contains(&".hc-black .monaco-select-box-dropdown-padding"));
        assert!(selectors.contains(&".hc-light .monaco-select-box-dropdown-padding"));
        std::fs::remove_dir_all(&root).ok();
    }
}
