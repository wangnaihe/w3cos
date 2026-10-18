use super::*;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/leak-lint/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/leak-lint")
}

/// (line, pattern, severity, fix)
type Row = (usize, &'static str, Severity, Option<&'static str>);

fn summary(name: &str) -> (Vec<Row>, String, FileResult) {
    let source = fixture(name);
    let result = lint_source(&source, name, &Options::default());
    assert!(result.parsed, "{name} parses");
    let rows = result
        .findings
        .iter()
        .map(|f| (f.line, f.pattern, f.severity, f.fix))
        .collect();
    let fixed = apply_edits(&source, &result.edits);
    (rows, fixed, result)
}

/// Re-linting fixed output offers no further rewrites and still parses.
fn assert_fix_is_stable(name: &str, fixed: &str) {
    let again = lint_source(fixed, name, &Options::default());
    assert!(again.parsed, "{name}: fixed output parses:\n{fixed}");
    assert!(
        again.edits.is_empty(),
        "{name}: fix is idempotent: {:?}",
        again.findings
    );
}

use Severity::{Error as E, Info as I, Warning as W};

#[test]
fn listener_registrations_without_dispose() {
    let (rows, fixed, _) = summary("listeners.ts");
    assert_eq!(
        rows,
        vec![
            (
                6,
                "listener-without-dispose",
                W,
                Some("register-disposable")
            ),
            (7, "listener-without-dispose", I, None),
            (9, "listener-without-dispose", W, None),
        ]
    );
    assert!(fixed.contains("\t\tthis._register(model.onDidChangeContent(() => this.render()));\n"));
    // already registered, removed in dispose(), or { once: true }: untouched
    assert!(fixed.contains("this._register(model.onWillSave(() => this.save()));"));
    assert!(!fixed.contains("this._register(this._register("));
    assert_fix_is_stable("listeners.ts", &fixed);
}

#[test]
fn back_pointers_become_weakref_accessors() {
    let (rows, fixed, _) = summary("back-pointers.ts");
    assert_eq!(
        rows,
        vec![
            (2, "back-pointer", W, Some("weak-back-pointer")),
            (6, "back-pointer", W, None),
            (12, "back-pointer", W, Some("weak-back-pointer")),
            (13, "back-pointer", W, None), // readonly: no setter possible
            (22, "back-pointer", W, None), // parameter property
        ]
    );
    assert!(fixed.contains("\tprivate _parentRef: WeakRef<object> | null | undefined;"));
    assert!(fixed.contains("\tget parent(): TreeNode | undefined { const ref = this._parentRef; return (ref instanceof WeakRef ? ref.deref() : ref) as TreeNode | undefined; }"));
    assert!(fixed.contains("\tprivate set owner(value: Editor) { this._ownerRef = value === null || value === undefined ? value : new WeakRef(value as object); }"));
    // primitive-typed `parent` and the write sites are left alone
    assert!(fixed.contains("parent: number = 0;"));
    assert!(fixed.contains("child.parent = this;"));
    assert_fix_is_stable("back-pointers.ts", &fixed);
    let again = lint_source(&fixed, "back-pointers.ts", &Options::default());
    let remaining: Vec<_> = again.findings.iter().map(|f| f.line).collect();
    // only the two unfixable members (readonly `host`, parameter property `container`) remain
    assert_eq!(remaining.len(), 2, "{:?}", again.findings);
}

#[test]
fn object_keyed_maps_become_weakmaps_only_without_iteration() {
    let (rows, fixed, _) = summary("maps.ts");
    assert_eq!(
        rows,
        vec![
            (4, "object-keyed-map", W, Some("weakmap")),
            (5, "object-keyed-map", W, None), // iterated, never evicted
            (25, "object-keyed-map", W, Some("weakmap")),
            (30, "object-keyed-map", W, None), // exported: other modules may iterate it
            (32, "object-keyed-map", W, None), // public field
        ]
    );
    assert!(fixed.contains("export const shared = new Map<IEditor, number>();"));
    assert!(fixed.contains("private readonly cache = new WeakMap<IEditor, string>();"));
    assert!(fixed.contains("private readonly all: Map<IEditor, number> = new Map();"));
    assert!(fixed.contains("private readonly byId = new Map<string, IEditor>();"));
    assert!(fixed.contains("private readonly tracked = new Map<IEditor, number>();"));
    assert!(fixed.contains("const views = new WeakMap<HTMLElement, number>();"));
    assert_fix_is_stable("maps.ts", &fixed);
}

#[test]
fn unregistered_disposables_are_wrapped_in_register() {
    let (rows, fixed, _) = summary("disposables.ts");
    assert_eq!(
        rows,
        vec![
            (
                5,
                "disposable-not-registered",
                W,
                Some("register-disposable")
            ),
            (
                12,
                "disposable-not-registered",
                W,
                Some("register-disposable")
            ),
            (18, "disposable-not-registered", W, None), // no _register in Session
        ]
    );
    assert!(fixed.contains("private readonly _onDidChange = this._register(new Emitter<void>());"));
    assert!(fixed.contains("this.store = this._register(new DisposableStore());"));
    assert!(fixed.contains("private readonly _onDidEnd = new Emitter<void>();"));
    assert_fix_is_stable("disposables.ts", &fixed);
}

#[test]
fn this_captures_and_w3ir_back_edges() {
    let (rows, _, result) = summary("this-capture.js");
    assert!(result.ir_lowered, "plain JS lowers to W3IR");
    assert_eq!(
        rows,
        vec![
            (4, "this-capture-stored-on-this", W, None),
            (5, "this-capture-stored-on-this", I, None),
            (10, "closure-back-edge", I, None),
            (16, "mutual-properties", W, None),
        ]
    );
    let back_edge = result
        .findings
        .iter()
        .find(|f| f.pattern == "closure-back-edge")
        .unwrap();
    assert!(
        back_edge.message.contains("captures `obj`"),
        "{}",
        back_edge.message
    );
}

#[test]
fn clean_code_has_no_findings() {
    let (rows, _, result) = summary("clean.ts");
    assert!(rows.is_empty(), "{:?}", result.findings);
    assert!(result.edits.is_empty());
}

#[test]
fn directory_report_text_json_and_exit_codes() {
    let report = lint_path(&fixtures_dir(), &Options::default());
    assert_eq!(report.files_scanned, 6);
    assert_eq!(report.parse_failures, 0);
    assert_eq!(report.count(E), 0);
    assert_eq!(report.exit_code(Some(W)), 1);
    assert_eq!(report.exit_code(Some(E)), 0);
    assert_eq!(report.exit_code(None), 0);
    let text = report.to_text();
    assert!(
        text.contains("maps.ts:4:2: warning [object-keyed-map]"),
        "{text}"
    );
    assert!(text.contains("(auto-fixable: --fix weakmap)"));
    assert!(
        text.lines()
            .last()
            .unwrap()
            .starts_with("leak-lint: 6 file(s), 0 parse failure(s)")
    );
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(json["files_scanned"], 6);
    assert_eq!(
        json["findings"].as_array().unwrap().len(),
        report.findings.len()
    );
    let first = &json["findings"][0];
    for key in [
        "file",
        "line",
        "column",
        "pattern",
        "severity",
        "message",
        "suggestion",
        "fix",
    ] {
        assert!(first.get(key).is_some(), "finding has {key}");
    }
    let diff = report.fix_diff();
    assert!(diff.contains("-\tprivate readonly cache = new Map<IEditor, string>();\n+\tprivate readonly cache = new WeakMap<IEditor, string>();\n"), "{diff}");
}

#[test]
fn write_fixes_applies_the_dry_run_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maps.ts");
    std::fs::write(&path, fixture("maps.ts")).unwrap();
    let report = lint_path(dir.path(), &Options::default());
    assert_eq!(report.write_fixes().unwrap(), 1);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("new WeakMap<IEditor, string>()"));
    let again = lint_path(dir.path(), &Options::default());
    assert!(again.fix_diff().is_empty());
}

#[test]
fn excludes_and_ir_toggle() {
    let options = Options {
        ir: false,
        exclude: vec!["this-capture".into()],
    };
    let report = lint_path(&fixtures_dir(), &options);
    assert_eq!(report.files_scanned, 5);
    assert_eq!(report.ir_lowered, 0);
    assert!(
        report
            .findings
            .iter()
            .all(|f| !f.file.contains("this-capture"))
    );
}

#[test]
fn unified_diff_hunks() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    let new = "a\nb\nC\nd\ne\nf\ng\nh\ni\nJ\n";
    let diff = unified_diff("x", old, new);
    assert_eq!(
        diff,
        "--- a/x\n+++ b/x\n@@ -1,6 +1,6 @@\n a\n b\n-c\n+C\n d\n e\n f\n@@ -7,4 +7,4 @@\n g\n h\n i\n-j\n+J\n"
    );
    assert!(unified_diff("x", old, old).is_empty());
}

#[test]
fn unparseable_files_are_counted_not_reported() {
    let result = lint_source("class {", "broken.ts", &Options::default());
    assert!(!result.parsed);
    assert!(result.findings.is_empty());
}
