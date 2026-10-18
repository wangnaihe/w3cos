//! `w3cos leak-lint` end to end: text/JSON output, exit codes, --fix dry run and --write.

use std::process::Command;

fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../w3cos-compiler/tests/fixtures/leak-lint")
}

fn w3cos(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_w3cos"))
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn text_report_and_exit_codes() {
    let dir = fixtures();
    let dir = dir.to_str().unwrap();
    let (code, stdout, _) = w3cos(&["leak-lint", dir]);
    assert_eq!(code, 1, "warnings fail by default:\n{stdout}");
    assert!(
        stdout.contains("maps.ts:4:2: warning [object-keyed-map]"),
        "{stdout}"
    );
    assert!(stdout.contains("leak-lint: 6 file(s)"));
    let (code, _, _) = w3cos(&["leak-lint", dir, "--fail-on", "error"]);
    assert_eq!(code, 0);
    let clean = format!("{dir}/clean.ts");
    let (code, stdout, _) = w3cos(&["leak-lint", &clean]);
    assert_eq!(code, 0, "{stdout}");
    let (code, _, stderr) = w3cos(&["leak-lint", dir, "--fail-on", "bogus"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("--fail-on"), "{stderr}");
}

#[test]
fn json_report() {
    let file = fixtures().join("listeners.ts");
    let (code, stdout, _) = w3cos(&[
        "leak-lint",
        file.to_str().unwrap(),
        "--json",
        "--fail-on",
        "never",
    ]);
    assert_eq!(code, 0);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["findings"][0]["pattern"], "listener-without-dispose");
    assert_eq!(json["findings"][0]["line"], 6);
    assert_eq!(json["summary"]["warning"], 2);
}

#[test]
fn fix_is_a_dry_run_unless_write() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("disposables.ts");
    let original = std::fs::read_to_string(fixtures().join("disposables.ts")).unwrap();
    std::fs::write(&file, &original).unwrap();
    let path = file.to_str().unwrap();
    let (_, stdout, _) = w3cos(&["leak-lint", path, "--fix", "--fail-on", "never"]);
    assert!(stdout.contains("--- a/disposables.ts"), "{stdout}");
    assert!(
        stdout.contains("+\tprivate readonly _onDidChange = this._register(new Emitter<void>());")
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        original,
        "dry run leaves the file alone"
    );
    let (_, _, stderr) = w3cos(&["leak-lint", path, "--fix", "--write", "--fail-on", "never"]);
    assert!(stderr.contains("rewrote 1 file(s)"), "{stderr}");
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("this.store = this._register(new DisposableStore());"));
}
