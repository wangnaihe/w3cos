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

/// A single flat rule ready for compiled-selector registration codegen.
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
        // CSS 2.1 invalidates an entire selector list when any member is
        // syntactically invalid. Validate before flattening the comma group,
        // otherwise a valid sibling selector would be applied on its own.
        if !selector_list_is_syntactically_valid(&rule.selectors) {
            continue;
        }
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

fn selector_list_is_syntactically_valid(selectors: &str) -> bool {
    selector_list_has_no_empty_members(selectors)
        && split_selector_group(selectors)
            .iter()
            .all(|selector| selector_is_syntactically_valid(selector))
        && w3cos_dom::stylesheet::compile_selector_bytecode(selectors).is_some()
}

fn selector_list_has_no_empty_members(selectors: &str) -> bool {
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut member_has_content = false;
    for character in selectors.chars() {
        match character {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            ',' if paren == 0 && bracket == 0 => {
                if !member_has_content {
                    return false;
                }
                member_has_content = false;
                continue;
            }
            _ => {}
        }
        member_has_content |= !character.is_whitespace();
    }
    member_has_content
}

fn selector_is_syntactically_valid(selector: &str) -> bool {
    let selector = selector.trim();
    if selector.is_empty() || selector.starts_with(|character: char| character.is_ascii_digit()) {
        return false;
    }
    let bytes = selector.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'[' {
            cursor += 1;
            continue;
        }
        let start = cursor + 1;
        let mut quote = None;
        cursor = start;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\'' | b'"' if quote == Some(bytes[cursor]) => quote = None,
                b'\'' | b'"' if quote.is_none() => quote = Some(bytes[cursor]),
                b']' if quote.is_none() => break,
                _ => {}
            }
            cursor += 1;
        }
        if cursor >= bytes.len() || quote.is_some() {
            return false;
        }
        let expression = selector[start..cursor].trim();
        if expression.is_empty() {
            return false;
        }
        let operator = ["~=", "|=", "^=", "$=", "*=", "="]
            .into_iter()
            .find_map(|operator| expression.split_once(operator));
        let (name, value) = operator.map_or((expression, None), |(name, value)| {
            (name, Some(value.trim()))
        });
        let name = name.trim();
        if name.starts_with(|character: char| character.is_ascii_digit()) {
            return false;
        }
        if value.is_some_and(str::is_empty) {
            return false;
        }
        cursor += 1;
    }
    true
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
    // A decoded UTF BOM is an encoding signature, not part of the first
    // selector or at-rule. Keeping U+FEFF here turns `.class` into a type
    // selector followed by a class selector and silently prevents matching.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let source = strip_cdo_cdc(&strip_block_comments(source));
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

        if bytes[pos] == b'@' && starts_at_keyword(bytes, pos) {
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
        let Some(block_start) = find_rule_block_start(bytes, pos) else {
            let tail = source[selector_start..].trim();
            if !tail.is_empty() {
                push_warning(
                    warnings,
                    format!("css {path}: truncated rule near `{}`", truncate(&tail, 40)),
                );
            }
            break;
        };
        pos = block_start;
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
            if selector_list_is_syntactically_valid(selector_str) {
                imports_allowed = false;
            }
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

fn find_rule_block_start(bytes: &[u8], mut pos: usize) -> Option<usize> {
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    while pos < bytes.len() {
        let byte = bytes[pos];
        if escaped {
            escaped = false;
            pos += 1;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            pos += 1;
            continue;
        }
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
            pos += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            pos += 1;
            continue;
        }
        if pos + 4 <= bytes.len() && bytes[pos..pos + 4].eq_ignore_ascii_case(b"url(") {
            pos = css_url_token_end(bytes, pos + 4);
            continue;
        }
        match byte {
            b'{' if delimiters.is_empty() => return Some(pos),
            b'(' => delimiters.push(b')'),
            b'[' => delimiters.push(b']'),
            b')' | b']' if delimiters.last() == Some(&byte) => {
                delimiters.pop();
            }
            _ => {}
        }
        pos += 1;
    }
    None
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
    let keyword = source[kw_start..pos].to_ascii_lowercase();

    // Find the first top-level ';' or '{'. Delimiters inside strings and
    // nested simple blocks belong to the at-rule prelude and do not terminate
    // it, including during error recovery for unknown at-rules.
    let Some(scan) = find_at_rule_terminator(bytes, pos) else {
        if keyword == "import" && imports_allowed {
            let prelude = close_css_value_at_eof(source[pos..].trim());
            if let Some(import) = parse_import_prelude(&prelude) {
                imports.push(import);
            }
        }
        return (bytes.len(), true);
    };
    let prelude = source[pos..scan].trim().to_string();

    if bytes[scan] == b';' {
        if keyword == "import" && imports_allowed {
            if let Some(import) = parse_import_prelude(&prelude) {
                imports.push(import);
            } else {
                push_warning(
                    warnings,
                    format!("css {path}: malformed @import skipped: {prelude}"),
                );
            }
        }
        return (scan + 1, true);
    }

    // At-rule with a block.
    pos = scan + 1;
    let (block_str, advance, _terminated) = extract_brace_content(&source[pos..]);
    pos += advance;
    match keyword.as_str() {
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
        "container" => {
            let first_nested_rule = rules.len();
            parse_block_into(
                block_str, path, warnings, rules, imports, font_faces, media, false,
            );
            for rule in &mut rules[first_nested_rule..] {
                rule.declarations
                    .push(("__w3cos_container_query".to_string(), prelude.clone()));
            }
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

fn find_at_rule_terminator(bytes: &[u8], mut pos: usize) -> Option<usize> {
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    while pos < bytes.len() {
        let byte = bytes[pos];
        if escaped {
            escaped = false;
            pos += 1;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            pos += 1;
            continue;
        }
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
            pos += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            pos += 1;
            continue;
        }
        match byte {
            b';' if delimiters.is_empty() => return Some(pos),
            b'{' if delimiters.is_empty() => return Some(pos),
            b'(' => delimiters.push(b')'),
            b'[' => delimiters.push(b']'),
            b'{' => delimiters.push(b'}'),
            b')' | b']' | b'}' if delimiters.last() == Some(&byte) => {
                delimiters.pop();
            }
            _ => {}
        }
        pos += 1;
    }
    None
}

fn starts_at_keyword(bytes: &[u8], at: usize) -> bool {
    let Some(first) = bytes.get(at + 1).copied() else {
        return false;
    };
    if first.is_ascii_alphabetic() || first == b'_' || first >= 0x80 || first == b'\\' {
        return true;
    }
    if first != b'-' {
        return false;
    }
    bytes.get(at + 2).is_some_and(|next| {
        next.is_ascii_alphabetic() || matches!(*next, b'-' | b'_' | b'\\') || *next >= 0x80
    })
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
    let mut delimiters = vec![b'}'];
    let mut quote = None;
    let mut escaped = false;
    let mut pos = 0;
    while pos < bytes.len() {
        let byte = bytes[pos];
        if escaped {
            escaped = false;
            pos += 1;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            pos += 1;
            continue;
        }
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
            pos += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            pos += 1;
            continue;
        }
        if pos + 4 <= bytes.len() && bytes[pos..pos + 4].eq_ignore_ascii_case(b"url(") {
            pos = css_url_token_end(bytes, pos + 4);
            continue;
        }
        match byte {
            b'{' => delimiters.push(b'}'),
            b'(' => delimiters.push(b')'),
            b'[' => delimiters.push(b']'),
            b'}' | b')' | b']' if delimiters.last() == Some(&byte) => {
                delimiters.pop();
                if delimiters.is_empty() {
                    return (&s[..pos], pos + 1, true);
                }
            }
            _ => {}
        }
        pos += 1;
    }
    (s, s.len(), false)
}

/// Strip `/* ... */` comments. (`//` is NOT a CSS comment — stripping it
/// would corrupt `url(...)` values.)
fn strip_block_comments(source: &str) -> String {
    let mut result = Vec::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut quote = None;
    let mut url_depth = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            result.extend_from_slice(&bytes[i..i + 2]);
            i += 2;
        } else if let Some(active_quote) = quote {
            result.push(bytes[i]);
            if bytes[i] == active_quote {
                quote = None;
            }
            i += 1;
        } else if matches!(bytes[i], b'\'' | b'"') {
            quote = Some(bytes[i]);
            result.push(bytes[i]);
            i += 1;
        } else if url_depth == 0
            && bytes[i..].len() >= 4
            && bytes[i..i + 4].eq_ignore_ascii_case(b"url(")
        {
            result.extend_from_slice(&bytes[i..i + 4]);
            url_depth = 1;
            i += 4;
        } else if url_depth > 0 {
            result.push(bytes[i]);
            match bytes[i] {
                b'(' => url_depth += 1,
                b')' => url_depth -= 1,
                _ => {}
            }
            i += 1;
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            if result.last().is_none_or(|byte| !byte.is_ascii_whitespace()) {
                result.push(b' ');
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(result).expect("comment stripping preserves valid UTF-8 boundaries")
}

fn strip_cdo_cdc(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut remaining = source;
    let mut quote = None;
    let mut escaped = false;
    while let Some(character) = remaining.chars().next() {
        if escaped {
            result.push(character);
            escaped = false;
            remaining = &remaining[character.len_utf8()..];
            continue;
        }
        if character == '\\' {
            result.push(character);
            escaped = true;
            remaining = &remaining[character.len_utf8()..];
            continue;
        }
        if let Some(active_quote) = quote {
            result.push(character);
            if character == active_quote {
                quote = None;
            }
            remaining = &remaining[character.len_utf8()..];
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            result.push(character);
            remaining = &remaining[character.len_utf8()..];
        } else if remaining.starts_with("<!--") {
            result.push(' ');
            remaining = &remaining[4..];
        } else if remaining.starts_with("-->") {
            result.push(' ');
            remaining = &remaining[3..];
        } else {
            result.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    result
}

/// Split a declaration block on top-level `;` (paren-aware, so `url(...;...)`
/// and `var(--x, a; b)` values survive), then on the first top-level `:`.
fn parse_declarations_raw(block: &str) -> Vec<(String, String)> {
    let mut declarations = Vec::new();
    let segments = split_top_level(block, b';')
        .into_iter()
        .flat_map(|segment| {
            recover_trailing_declaration(&segment)
                .map(|(prefix, suffix)| vec![prefix, suffix])
                .unwrap_or_else(|| vec![segment])
        })
        .collect::<Vec<_>>();
    for segment in segments {
        let segment = segment.trim();
        // An at-keyword cannot start a declaration. Its balanced blocks
        // belong to this malformed segment, not to a new declaration after
        // a closing brace; resume only at the top-level semicolon.
        if segment.is_empty() || segment.starts_with('@') {
            continue;
        }
        let mut delimiters = Vec::new();
        let mut quote = None;
        let mut escaped = false;
        let mut colon = None;
        for (i, ch) in segment.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if let Some(active_quote) = quote {
                if ch == active_quote {
                    quote = None;
                }
                continue;
            }
            match ch {
                '\'' | '"' => quote = Some(ch),
                '(' => delimiters.push(')'),
                '[' => delimiters.push(']'),
                '{' => delimiters.push('}'),
                ')' | ']' | '}' if delimiters.last() == Some(&ch) => {
                    delimiters.pop();
                }
                ':' if delimiters.is_empty() => {
                    colon = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(colon) = colon else {
            continue; // not a `prop: value` pair — tolerated, skipped
        };
        let raw_property = segment[..colon].trim();
        let Some(prop) =
            w3cos_dom::stylesheet::css_unescape_identifier(raw_property).or_else(|| {
                // A malformed declaration may contain a balanced at-rule block.
                // Once that block closes, a following identifier starts a new
                // declaration even without an intervening semicolon.
                raw_property.rsplit_once('}').and_then(|(_, suffix)| {
                    w3cos_dom::stylesheet::css_unescape_identifier(suffix.trim())
                })
            })
        else {
            continue;
        };
        let prop = prop.to_ascii_lowercase();
        let raw_value = close_css_value_at_eof(segment[colon + 1..].trim());
        // Escapes that decode to CSS whitespace remain part of an identifier
        // token; they must not turn into surrounding whitespace that a later
        // value parser trims away (for example `red\9` is not the `red`
        // keyword). Keep those spellings escaped until a token-aware value
        // parser can consume them.
        let value = if contains_escaped_css_whitespace(&raw_value) {
            raw_value
        } else {
            w3cos_dom::stylesheet::css_unescape_identifier(&raw_value).unwrap_or(raw_value)
        };
        if !prop.is_empty() && !value.is_empty() && declaration_priority_is_valid(&value) {
            declarations.push((prop, value));
        }
    }
    declarations
}

fn recover_trailing_declaration(segment: &str) -> Option<(String, String)> {
    if !has_unmatched_closing_delimiter(segment) {
        return None;
    }
    for (line_start, _) in segment.match_indices('\n').rev() {
        let suffix = segment[line_start + 1..].trim();
        let Some(colon) = suffix.find(':') else {
            continue;
        };
        if w3cos_dom::stylesheet::css_unescape_identifier(suffix[..colon].trim()).is_some()
            && !suffix[colon + 1..].trim().is_empty()
        {
            return Some((segment[..line_start].to_string(), suffix.to_string()));
        }
    }
    None
}

fn has_unmatched_closing_delimiter(value: &str) -> bool {
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            continue;
        }
        match character {
            '(' => delimiters.push(')'),
            '[' => delimiters.push(']'),
            '{' => delimiters.push('}'),
            ')' | ']' | '}' if delimiters.last() == Some(&character) => {
                delimiters.pop();
            }
            ')' | ']' | '}' => return true,
            _ => {}
        }
    }
    false
}

fn contains_escaped_css_whitespace(value: &str) -> bool {
    let characters = value.chars().collect::<Vec<_>>();
    let mut pos = 0usize;
    while pos < characters.len() {
        if characters[pos] != '\\' {
            pos += 1;
            continue;
        }
        let Some(next) = characters.get(pos + 1).copied() else {
            break;
        };
        if matches!(next, '\n' | '\r' | '\u{000c}') {
            return true;
        }
        if next.is_ascii_hexdigit() {
            let mut end = pos + 1;
            while end < characters.len() && end < pos + 7 && characters[end].is_ascii_hexdigit() {
                end += 1;
            }
            let digits = characters[pos + 1..end].iter().collect::<String>();
            if u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
                .is_some_and(|character| {
                    matches!(
                        character,
                        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' '
                    )
                })
            {
                return true;
            }
            pos = end;
        } else {
            pos += 2;
        }
    }
    false
}

fn declaration_priority_is_valid(value: &str) -> bool {
    value
        .rfind('!')
        .is_none_or(|marker| value[marker + 1..].trim().eq_ignore_ascii_case("important"))
}

fn close_css_value_at_eof(value: &str) -> String {
    let mut quote = None;
    let mut escaped = false;
    let mut delimiters = Vec::new();
    let bytes = value.as_bytes();
    let mut pos = 0usize;
    let mut url_eof_closure = None;
    while pos < value.len() {
        let character = value[pos..].chars().next().expect("character boundary");
        if escaped {
            escaped = false;
            pos += character.len_utf8();
            continue;
        }
        if character == '\\' {
            escaped = true;
            pos += character.len_utf8();
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            }
            pos += character.len_utf8();
            continue;
        }
        if pos + 4 <= bytes.len() && bytes[pos..pos + 4].eq_ignore_ascii_case(b"url(") {
            let token_start = pos + 4;
            pos = css_url_token_end(bytes, token_start);
            if pos == bytes.len() && bytes.last() != Some(&b')') {
                url_eof_closure = css_url_eof_closure(&bytes[token_start..]);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => delimiters.push(')'),
            '[' => delimiters.push(']'),
            ')' | ']' if delimiters.last() == Some(&character) => {
                delimiters.pop();
            }
            _ => {}
        }
        pos += character.len_utf8();
    }
    let mut closed = value.to_string();
    if let Some(active_quote) = quote {
        closed.push(active_quote);
    }
    closed.extend(delimiters.into_iter().rev());
    if let Some(closure) = url_eof_closure {
        closed.push_str(closure);
    }
    closed
}

fn css_url_eof_closure(mut bytes: &[u8]) -> Option<&'static str> {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    let Some(&first) = bytes.first() else {
        return Some(")");
    };
    if matches!(first, b'\'' | b'"') {
        let quote = first;
        let mut escaped = false;
        for (index, &byte) in bytes[1..].iter().enumerate() {
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                continue;
            }
            if matches!(byte, b'\n' | b'\r' | b'\x0c') {
                return None;
            }
            if byte == quote {
                return bytes[index + 2..]
                    .iter()
                    .all(u8::is_ascii_whitespace)
                    .then_some(")");
            }
        }
        return Some(if quote == b'"' { "\")" } else { "')" });
    }

    let mut escaped = false;
    for &byte in bytes {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
        } else if matches!(byte, b'\'' | b'"' | b'(' | b'\0'..=b'\x08' | b'\x0b' | b'\x0e'..=b'\x1f' | b'\x7f')
        {
            return None;
        }
    }
    Some(")")
}

fn split_top_level(s: &str, sep: u8) -> Vec<String> {
    let mut parts = Vec::new();
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let bytes = s.as_bytes();
    let mut start = 0usize;
    let mut pos = 0usize;
    while pos < bytes.len() {
        let ch = bytes[pos] as char;
        if escaped {
            escaped = false;
            pos += 1;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            pos += 1;
            continue;
        }
        if pos + 4 <= bytes.len() && bytes[pos..pos + 4].eq_ignore_ascii_case(b"url(") {
            pos = css_url_token_end(bytes, pos + 4);
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => escaped = true,
            '(' => delimiters.push(')'),
            '[' => delimiters.push(']'),
            '{' => delimiters.push('}'),
            ')' | ']' | '}' if delimiters.last() == Some(&ch) => {
                delimiters.pop();
            }
            _ => {}
        }
        if ch == sep as char && delimiters.is_empty() {
            parts.push(s[start..pos].to_string());
            start = pos + 1;
        }
        pos += 1;
    }
    parts.push(s[start..].to_string());
    parts
}

fn css_url_token_end(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let mut delimiters = vec![b')'];
    let mut quote = None;
    let mut structured = false;
    while pos < bytes.len() {
        let byte = bytes[pos];
        if byte == b'\\' && pos + 1 < bytes.len() {
            pos += 2;
            continue;
        }
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            } else if matches!(byte, b'\n' | b'\r' | b'\x0c') {
                // A bad string token ends at a newline, but the surrounding
                // url( function remains open and continues consuming values.
                quote = None;
            }
            pos += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            if !structured {
                structured = true;
            }
            quote = Some(byte);
            pos += 1;
            continue;
        }
        if byte == b'(' {
            structured = true;
            delimiters.push(b')');
        } else if structured && byte == b'{' {
            delimiters.push(b'}');
        } else if matches!(byte, b')' | b'}') && delimiters.last() == Some(&byte) {
            delimiters.pop();
            if delimiters.is_empty() {
                return pos + 1;
            }
        }
        pos += 1;
    }
    pos
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
        let end = s
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= max)
            .last()
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate("平和 selector", 2), "...");
        assert_eq!(truncate("平和 selector", 3), "平...");
    }

    #[test]
    fn brace_extraction_ignores_strings_escapes_and_nested_delimiters() {
        for source in [
            r#"\} color: red; } trailing"#,
            r#"content: "}"; } trailing"#,
            r#"value: ( } ); color: red; } trailing"#,
            r#"value: [ } ]; color: red; } trailing"#,
        ] {
            let (block, consumed, terminated) = extract_brace_content(source);
            assert!(terminated, "source: {source}");
            assert!(block.contains("color: red") || block.contains("content"));
            assert_eq!(&source[consumed..], " trailing");
        }
    }

    #[test]
    fn escaped_comment_opener_does_not_start_a_comment() {
        assert_eq!(
            strip_block_comments(r"\/*;color: green;*/"),
            r"\/*;color: green;*/"
        );
        assert_eq!(strip_block_comments("a/* hidden */b"), "a b");
    }

    #[test]
    fn invalid_selector_member_discards_complete_css_rule() {
        let sheet = parse_css_source(
            "[1digit], div { color: red; } [title~=], p.valid { color: red; } body,,main { color: red; }",
            "invalid.css",
        );
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn invalid_at_prefix_recovers_at_the_following_rule_block() {
        for prefix in [
            "@ import \"red.css\";",
            "@1import \"red.css\";",
            "@-1import \"red.css\";",
        ] {
            let source = format!("{prefix} div {{ color: red; }} * {{ color: green; }}");
            let sheet = parse_css_source(&source, "invalid-at.css");
            assert_eq!(
                sheet
                    .rules
                    .iter()
                    .map(|rule| rule.selector.as_str())
                    .collect::<Vec<_>>(),
                ["*"],
                "source: {source}"
            );
        }
    }

    #[test]
    fn invalid_rules_do_not_close_the_leading_import_window() {
        for source in [
            "@bad-rule value; @import \"green.css\";",
            "1badselector { bad: value; } @import \"green.css\";",
        ] {
            let sheet = parse_css_source(source, "import-recovery.css");
            assert_eq!(sheet.imports.len(), 1, "source: {source}");
            assert_eq!(sheet.imports[0].href, "green.css");
        }
    }

    #[test]
    fn unknown_at_rule_recovery_skips_nested_delimiters() {
        let sheet = parse_css_source(
            "@media all { @foo [; #bad { color: red; }] (; #bad { color: red; }); #good { color: green; } }",
            "at-rule-recovery.css",
        );
        assert_eq!(
            sheet
                .rules
                .iter()
                .map(|rule| rule.selector.as_str())
                .collect::<Vec<_>>(),
            ["#good"]
        );
    }

    #[test]
    fn eof_closes_functions_strings_and_import_rules() {
        assert_eq!(close_css_value_at_eof("rgb(0, 128, 0"), "rgb(0, 128, 0)");
        assert_eq!(close_css_value_at_eof("\"Filler Text"), "\"Filler Text\"");
        assert_eq!(
            close_css_value_at_eof("url(\"support/swatch-green.png"),
            "url(\"support/swatch-green.png\")"
        );
        assert_eq!(
            close_css_value_at_eof("url(support/swatch-green.png"),
            "url(support/swatch-green.png)"
        );

        let declaration = parse_css_source("div { color: rgb(0, 128, 0", "eof.css");
        assert_eq!(declaration.rules[0].declarations[0].1, "rgb(0, 128, 0)");
        for source in [
            "@import \"support/eof-green.css",
            "@import \"support/eof-green.css\"",
        ] {
            let sheet = parse_css_source(source, "eof-import.css");
            assert_eq!(sheet.imports.len(), 1, "source: {source}");
            assert_eq!(sheet.imports[0].href, "support/eof-green.css");
        }
    }

    #[test]
    fn invalid_priority_tokens_discard_the_declaration() {
        let sheet = parse_css_source(
            "p { color: red ! fail; background: red ! important fail; width: 1px ! IMPORTANT; }",
            "priority.css",
        );
        assert_eq!(
            sheet.rules[0].declarations,
            [("width".into(), "1px ! IMPORTANT".into())]
        );
    }

    #[test]
    fn nested_blocks_do_not_leak_declarations_into_the_parent_rule() {
        let sheet = parse_css_source(
            ".test { test { :nested; color: yellow; background: red; }: ignored; text-decoration: underline; }",
            "nested-declaration.css",
        );
        assert!(
            sheet.rules[0]
                .declarations
                .iter()
                .all(|(property, _)| property != "color" && property != "background")
        );
        assert!(
            sheet.rules[0]
                .declarations
                .iter()
                .any(|(property, value)| property == "text-decoration" && value == "underline")
        );
    }

    #[test]
    fn malformed_at_rule_in_declarations_does_not_swallow_the_next_rule() {
        let sheet = parse_css_source(
            "#e { color: green; @foo [ color: red; } #e { color: red; } ] } #f { color: green; color: red @import \"red.css\"; }",
            "declaration-at-rule.css",
        );
        let f = sheet
            .rules
            .iter()
            .find(|rule| rule.selector == "#f")
            .expect("#f rule survives malformed predecessor");
        assert_eq!(f.declarations[0], ("color".into(), "green".into()));
    }

    #[test]
    fn malformed_at_rule_declaration_recovers_only_after_its_semicolon() {
        let sheet = parse_css_source(
            "#c { color: green; @media { #c { color: red !important } } color: red; }
             #d { color: red; @media { #d { color: red !important } }; color: green; }
             #a { color: green; @import 'red.css' color: red; }",
            "malformed-declaration.css",
        );
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(
            sheet.rules[0].declarations,
            vec![("color".into(), "green".into())]
        );
        assert_eq!(
            sheet.rules[1].declarations.last(),
            Some(&("color".into(), "green".into()))
        );
        assert_eq!(
            sheet.rules[2].declarations,
            vec![("color".into(), "green".into())]
        );
        assert!(sheet.imports.is_empty());
    }

    #[test]
    fn quoted_empty_attribute_value_remains_valid() {
        let sheet = parse_css_source(
            "[title~=\"\"], p.valid { color: green; }",
            "empty-token.css",
        );
        assert_eq!(sheet.rules.len(), 2);
    }

    #[test]
    fn quoted_semicolons_do_not_split_authored_declarations() {
        let sheet = parse_css_source(
            ".test::before { content: 'TEST &#x46;&#x41;&#x49;&#x4c;'; color: red; }",
            "quoted-semicolon.css",
        );

        assert_eq!(
            sheet.rules[0].declarations,
            vec![
                (
                    "content".to_string(),
                    "'TEST &#x46;&#x41;&#x49;&#x4c;'".to_string(),
                ),
                ("color".to_string(), "red".to_string()),
            ]
        );
    }

    #[test]
    fn escaped_whitespace_does_not_become_trimmable_keyword_whitespace() {
        let sheet = parse_css_source(
            ".test { color: green; color: red\\9; background: \\0020red; }",
            "test.css",
        );
        assert_eq!(
            sheet.rules[0].declarations,
            [
                ("color".to_string(), "green".to_string()),
                ("color".to_string(), "red\\9".to_string()),
                ("background".to_string(), "\\0020red".to_string()),
            ]
        );
    }

    #[test]
    fn declaration_recovery_resumes_after_balanced_malformed_blocks() {
        let sheet = parse_css_source(
            r#"p {
                background: red;
                color: green;
                color: red ] ) test-token \
                 [\]\5D ']' "]"; background: red; } p { color: red; } ]
                 (\)\29 ')' ")"; background: red; } p { color: red; } )
                 '\'; background: red; } p { color: red; }',
                 "\"; background: red; } p { color: red; }' p { color: red; } "
                background: white;
            }"#,
            "matching-brackets.css",
        );
        assert_eq!(
            sheet.rules[0].declarations.last(),
            Some(&("background".to_string(), "white".to_string()))
        );
    }

    #[test]
    fn bad_url_braces_do_not_capture_following_rules() {
        let sheet = parse_css_source(
            "p { color: red; border: solid red; background: red url( { test ); border: solid green; } p { color: green; }",
            "bad-url.css",
        );
        assert_eq!(sheet.rules.len(), 2, "{:#?}", sheet.rules);
        assert_eq!(
            sheet.rules[0].declarations.last(),
            Some(&("border".to_string(), "solid green".to_string()))
        );
        assert_eq!(
            sheet.rules[1].declarations.last(),
            Some(&("color".to_string(), "green".to_string()))
        );
    }

    #[test]
    fn bad_url_recovery_preserves_only_reachable_following_rules() {
        let cases = [
            (
                "#three { background-color: green; } #foo { background: url(foo\"bar) }\n#three { background-color: red; }",
                "#three",
                "green",
            ),
            (
                "#foo { background: url(foo\"bar) }\n) }\n#four { background-color: green; }",
                "#four",
                "green",
            ),
            (
                "#twelve { background: url(}{\"\"{)}); background-color: green; }",
                "#twelve",
                "green",
            ),
            (
                "#fourteen { background-color: green; } #foo { background: url(() }\n#fourteen { background-color: red; }",
                "#fourteen",
                "green",
            ),
        ];
        for (source, selector, expected) in cases {
            let sheet = parse_css_source(source, "bad-url-recovery.css");
            let values = sheet
                .rules
                .iter()
                .filter(|rule| rule.selector == selector)
                .flat_map(|rule| rule.declarations.iter())
                .filter(|(property, _)| property == "background-color")
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                values,
                [expected],
                "source={source}\nrules={:#?}",
                sheet.rules
            );
        }
        let bracket_uri =
            parse_css_source("#eleven { background: url([) green; }", "bracket-uri.css");
        assert_eq!(
            bracket_uri.rules[0].declarations,
            [("background".to_string(), "url([) green".to_string())]
        );
    }

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
    fn container_query_group_retains_inner_rules() {
        let sheet = parse_css_source(
            "@container semantic-surface (min-width: 44rem) {\
                .wide { grid-template-columns: 1fr 1fr; }\
            }\
            @container semantic-surface (max-width: 30rem) {\
                .semantic-grid[data-collapse='auto'] { --semantic-grid-columns: 1; }\
            }",
            "container.css",
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].selector, ".wide");
        assert_eq!(
            sheet.rules[0].declarations,
            vec![
                ("grid-template-columns".to_string(), "1fr 1fr".to_string()),
                (
                    "__w3cos_container_query".to_string(),
                    "semantic-surface (min-width: 44rem)".to_string(),
                ),
            ]
        );
        assert_eq!(
            sheet.rules[1].selector,
            ".semantic-grid[data-collapse='auto']"
        );
        assert_eq!(
            sheet.rules[1].declarations,
            vec![
                ("--semantic-grid-columns".to_string(), "1".to_string()),
                (
                    "__w3cos_container_query".to_string(),
                    "semantic-surface (max-width: 30rem)".to_string(),
                ),
            ]
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

    #[test]
    fn decoded_bom_does_not_prefix_the_first_unicode_selector() {
        let sheet = parse_css_source(
            "\u{feff}@charset \"UTF-8\"; .平和, #div2 { color: green; }",
            "bom.css",
        );
        assert_eq!(
            sheet
                .rules
                .iter()
                .map(|rule| rule.selector.as_str())
                .collect::<Vec<_>>(),
            [".平和", "#div2"]
        );
    }
}
