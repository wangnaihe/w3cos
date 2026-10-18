//! `w3cos leak-lint`: static detection of reference-cycle-prone patterns.
//!
//! w3cos keeps reference counting and breaks cycles with `WeakRef` (no
//! tracing GC, docs/roadmap/aot-port.md M1 item 8), so every strong cycle in
//! application code is a permanent leak. This lint flags the shapes that
//! usually create one, with `file:line`, a severity and a suggested fix:
//!
//! | pattern | what |
//! | --- | --- |
//! | `listener-without-dispose` | `addEventListener` with no matching `removeEventListener`, or an `onDidX(...)` / `.event(...)` registration whose `IDisposable` is discarded |
//! | `this-capture-stored-on-this` | `this.f = () => this.x`, `this.f = this.m.bind(this)`, arrow class fields |
//! | `back-pointer` | `child.parent = this`, or a `parent`/`owner`/... field written from outside |
//! | `object-keyed-map` | `Map` keyed by objects used as a cache (WeakMap candidate) or without eviction |
//! | `disposable-not-registered` | `new Emitter()` / `new DisposableStore()` ... in a disposable class that is never registered or disposed |
//! | `closure-back-edge` | W3IR `unique_back_edges` match: AOT already rewrites this capture to `WeakRef` |
//! | `mutual-properties` / `cross-file-cycle` | W3IR `unique_back_edges` ambiguity warnings |
//!
//! `--fix` computes mechanical rewrites (printed as a unified diff unless
//! `--write` is given):
//! - back-pointer field -> `WeakRef` storage with a `deref()` accessor pair
//! - object-keyed `Map` used only via get/set/has/delete -> `WeakMap`
//! - discarded listener registrations / unregistered disposables in a
//!   `Disposable` subclass -> wrapped in `this._register(...)`
//!
//! Every rule is heuristic and per file: it never resolves types or imports.
//! Fixes are only offered where the rewrite keeps the file's observable
//! behaviour apart from the lifetime change itself.

mod ir;
mod rules;

use serde::Serialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn parse(text: &str) -> Option<Option<Self>> {
        match text {
            "info" => Some(Some(Severity::Info)),
            "warning" => Some(Some(Severity::Warning)),
            "error" => Some(Some(Severity::Error)),
            "never" | "none" => Some(None),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub pattern: &'static str,
    pub severity: Severity,
    pub message: String,
    pub suggestion: String,
    /// Name of the `--fix` codemod that rewrites this finding, if any.
    pub fix: Option<&'static str>,
}

/// A byte-range replacement in one source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct FileResult {
    pub findings: Vec<Finding>,
    pub edits: Vec<Edit>,
    pub parsed: bool,
    pub ir_lowered: bool,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Run the W3IR `unique_back_edges` pass (slower; many TS files do not lower).
    pub ir: bool,
    /// Path substrings to skip, in addition to the defaults.
    pub exclude: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ir: true,
            exclude: Vec::new(),
        }
    }
}

/// Lint one source text. `file` is used for reporting and to pick TS vs JS.
pub fn lint_source(source: &str, file: &str, options: &Options) -> FileResult {
    let mut result = rules::lint(source, file);
    if options.ir && result.parsed {
        let (lowered, findings) = ir::lint(source, file);
        result.ir_lowered = lowered;
        result.findings.extend(findings);
    }
    result
        .findings
        .sort_by(|a, b| (a.line, a.column, a.pattern).cmp(&(b.line, b.column, b.pattern)));
    result
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub root: String,
    pub files_scanned: usize,
    pub parse_failures: usize,
    pub ir_lowered: usize,
    pub findings: Vec<Finding>,
    #[serde(skip)]
    pub edits: Vec<(PathBuf, String, String, Vec<Edit>)>,
}

const DEFAULT_EXCLUDES: &[&str] = &[
    "/node_modules/",
    "/.git/",
    "/out/",
    "/out-build/",
    "/dist/",
    "/target/",
    ".d.ts",
    ".min.js",
];

fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
    )
}

fn collect_files(root: &Path, options: &Options, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        out.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let text = format!("{}/", path.to_string_lossy().replace('\\', "/"));
        if DEFAULT_EXCLUDES
            .iter()
            .map(|s| s.to_string())
            .chain(options.exclude.iter().cloned())
            .any(|pattern| {
                text.contains(&pattern) || text.trim_end_matches('/').ends_with(&pattern)
            })
        {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, options, out);
        } else if is_source(&path) {
            out.push(path);
        }
    }
}

/// Lint every JS/TS file under `root` (or `root` itself if it is a file).
pub fn lint_path(root: &Path, options: &Options) -> Report {
    let mut files = Vec::new();
    collect_files(root, options, &mut files);
    let base = if root.is_file() {
        root.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        root.to_path_buf()
    };
    let mut report = Report {
        root: root.display().to_string(),
        ..Report::default()
    };
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let display = path
            .strip_prefix(&base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let result = lint_source(&source, &display, options);
        report.files_scanned += 1;
        if !result.parsed {
            report.parse_failures += 1;
        }
        if result.ir_lowered {
            report.ir_lowered += 1;
        }
        report.findings.extend(result.findings);
        if !result.edits.is_empty() {
            report.edits.push((path, display, source, result.edits));
        }
    }
    report
}

impl Report {
    pub fn count(&self, severity: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }

    pub fn by_pattern(&self) -> Vec<(&'static str, usize, usize)> {
        let mut counts: std::collections::BTreeMap<&'static str, (usize, usize)> =
            Default::default();
        for finding in &self.findings {
            let entry = counts.entry(finding.pattern).or_default();
            entry.0 += 1;
            if finding.fix.is_some() {
                entry.1 += 1;
            }
        }
        counts.into_iter().map(|(p, (n, f))| (p, n, f)).collect()
    }

    /// Exit status for CI: non-zero when any finding is at or above `fail_on`.
    pub fn exit_code(&self, fail_on: Option<Severity>) -> i32 {
        match fail_on {
            Some(level) if self.findings.iter().any(|f| f.severity >= level) => 1,
            _ => 0,
        }
    }

    pub fn summary_line(&self) -> String {
        let patterns = self
            .by_pattern()
            .iter()
            .map(|(p, n, f)| {
                format!(
                    "{p}={n}{}",
                    if *f > 0 {
                        format!(" ({f} fixable)")
                    } else {
                        String::new()
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "leak-lint: {} file(s), {} parse failure(s), {} lowered to W3IR; {} finding(s): {} error, {} warning, {} info{}{}",
            self.files_scanned,
            self.parse_failures,
            self.ir_lowered,
            self.findings.len(),
            self.count(Severity::Error),
            self.count(Severity::Warning),
            self.count(Severity::Info),
            if patterns.is_empty() { "" } else { "; " },
            patterns
        )
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = writeln!(
                out,
                "{}:{}:{}: {} [{}] {}",
                f.file,
                f.line,
                f.column,
                f.severity.as_str(),
                f.pattern,
                f.message
            );
            let _ = writeln!(
                out,
                "    fix: {}{}",
                f.suggestion,
                f.fix
                    .map(|name| format!(" (auto-fixable: --fix {name})"))
                    .unwrap_or_default()
            );
        }
        out.push_str(&self.summary_line());
        out.push('\n');
        out
    }

    pub fn to_json(&self) -> String {
        let value = serde_json::json!({
            "root": self.root,
            "files_scanned": self.files_scanned,
            "parse_failures": self.parse_failures,
            "ir_lowered": self.ir_lowered,
            "summary": {
                "error": self.count(Severity::Error),
                "warning": self.count(Severity::Warning),
                "info": self.count(Severity::Info),
                "by_pattern": self.by_pattern().iter().map(|(p, n, f)| serde_json::json!({"pattern": p, "count": n, "fixable": f})).collect::<Vec<_>>(),
            },
            "findings": self.findings,
        });
        serde_json::to_string_pretty(&value).expect("leak-lint report serializes")
    }

    /// Unified diff of every `--fix` rewrite (dry run).
    pub fn fix_diff(&self) -> String {
        let mut out = String::new();
        for (_, display, source, edits) in &self.edits {
            let fixed = apply_edits(source, edits);
            out.push_str(&unified_diff(display, source, &fixed));
        }
        out
    }

    /// Apply every `--fix` rewrite in place. Returns the number of files written.
    pub fn write_fixes(&self) -> std::io::Result<usize> {
        for (path, _, source, edits) in &self.edits {
            std::fs::write(path, apply_edits(source, edits))?;
        }
        Ok(self.edits.len())
    }
}

/// Apply non-overlapping edits (overlapping later edits are dropped).
pub fn apply_edits(source: &str, edits: &[Edit]) -> String {
    let mut edits = edits.to_vec();
    edits.sort_by_key(|e| (e.start, e.end));
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    for edit in edits {
        if edit.start < at || edit.end > source.len() {
            continue;
        }
        out.push_str(&source[at..edit.start]);
        out.push_str(&edit.text);
        at = edit.end;
    }
    out.push_str(&source[at..]);
    out
}

/// Minimal unified diff (LCS on lines, 3 lines of context).
pub fn unified_diff(name: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    // Trim the common prefix/suffix so the LCS table stays small.
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut lcs = vec![vec![0u32; bm.len() + 1]; am.len() + 1];
    for i in (0..am.len()).rev() {
        for j in (0..bm.len()).rev() {
            lcs[i][j] = if am[i] == bm[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    // ops: (tag, old_index, new_index) over the whole file
    let mut ops: Vec<(char, usize, usize)> = (0..prefix).map(|i| (' ', i, i)).collect();
    let (mut i, mut j) = (0, 0);
    while i < am.len() || j < bm.len() {
        if i < am.len() && j < bm.len() && am[i] == bm[j] {
            ops.push((' ', prefix + i, prefix + j));
            i += 1;
            j += 1;
        } else if i == am.len() || (j < bm.len() && lcs[i][j + 1] > lcs[i + 1][j]) {
            ops.push(('+', prefix + i, prefix + j));
            j += 1;
        } else {
            ops.push(('-', prefix + i, prefix + j));
            i += 1;
        }
    }
    for k in 0..suffix {
        ops.push((' ', a.len() - suffix + k, b.len() - suffix + k));
    }
    let mut out = format!("--- a/{name}\n+++ b/{name}\n");
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| op.0 != ' ')
        .map(|(k, _)| k)
        .collect();
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(3);
        let mut end = changed[k];
        while k < changed.len() && changed[k] <= end + 6 {
            end = changed[k];
            k += 1;
        }
        let end = (end + 4).min(ops.len());
        let hunk = &ops[start..end];
        let old_start = hunk
            .iter()
            .find(|o| o.0 != '+')
            .map(|o| o.1)
            .unwrap_or(hunk[0].1);
        let new_start = hunk
            .iter()
            .find(|o| o.0 != '-')
            .map(|o| o.2)
            .unwrap_or(hunk[0].2);
        let old_len = hunk.iter().filter(|o| o.0 != '+').count();
        let new_len = hunk.iter().filter(|o| o.0 != '-').count();
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            old_start + 1,
            old_len,
            new_start + 1,
            new_len
        );
        for (tag, oi, ni) in hunk {
            let line = if *tag == '+' { b[*ni] } else { a[*oi] };
            out.push(*tag);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
