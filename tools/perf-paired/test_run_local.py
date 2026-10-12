"""Local runner tests with synthetic files and mocked subprocesses; no browser/build/network."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import run_local as local
from validate import digest, file_digest


class LocalRunnerTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        self.repo = self.root / "repo"
        self.here = self.repo / "tools/perf-paired"
        (self.here / "locks").mkdir(parents=True)
        self.here.joinpath("manifest.json").write_bytes(local.HERE.joinpath("manifest.json").read_bytes())
        self.here.joinpath("scenarios.ts").write_text("synthetic fixture, never executed")
        self.lock = self.here / "locks/native.Cargo.lock"
        self.lock.write_text('version = 4\n[[package]]\nname = "synthetic"\nversion = "1.0.26"\n')
        self.cli = self.repo / "target/release/w3cos"
        self.cli.parent.mkdir(parents=True)
        self.cli.write_text("synthetic CLI, never executed")
        self.cli.chmod(0o755)
        self.browser = self.root / "Google Chrome.app/Contents/MacOS/Google Chrome"
        self.browser.parent.mkdir(parents=True)
        self.browser.write_text("synthetic browser, never launched")
        self.browser.chmod(0o755)
        self.output = self.root / "repo-perf-results/local-test"
        self.calls = []
        self.status = "pass"
        self.failed_stage = None
        self.probe_failure = None
        self.fetch_seen = False
        self.ignored = True
        self.interrupt = False
        self.corrupt_rebuilt_lock = False
        for target, kwargs in [
            ("run_local.REPO", {"new": self.repo}),
            ("run_local.HERE", {"new": self.here}),
            ("run_local.platform.system", {"return_value": "Darwin"}),
            ("run_local.subprocess.run", {"side_effect": self.subprocess}),
        ]:
            patched = patch(target, **kwargs)
            patched.start()
            self.addCleanup(patched.stop)
        environment = patch.dict(os.environ, {"PATH": str(self.root), "CARGO_HOME": str(self.root / "cargo-home")}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        # Suppress the runner's progress messages; subprocess diagnostics are
        # still written to real temporary files and asserted below.
        redirect = contextlib.redirect_stderr(io.StringIO())
        redirect.__enter__()
        self.addCleanup(redirect.__exit__, None, None, None)

    def args(self, *extra):
        return local.arguments(["--chromium", str(self.browser), "--output-directory", str(self.output), *extra])

    def subprocess(self, command, **kwargs):
        self.calls.append((command, kwargs))
        if kwargs.get("capture_output"):
            text = ""
            if command[0] == "git":
                if "check-ignore" in command:
                    return subprocess.CompletedProcess(command, 0 if self.ignored else 1, "", "")
                text = str(self.repo)
            elif command[0] in {"cargo", "rustc"}:
                text = command[0] + " synthetic version"
            elif command[0] == "xcode-select":
                text = "/Library/Developer/CommandLineTools"
            elif command[0] == str(self.cli):
                text = "--release --output" if "--help" in command else "w3cos synthetic version"
            elif "-c" in command:
                text = "1.55.0 synthetic Playwright"
            else:
                self.fail("Unexpected preflight subprocess: " + repr(command))
            if self.probe_failure and self.probe_failure in " ".join(command):
                return subprocess.CompletedProcess(command, 1, "", "synthetic missing prerequisite")
            return subprocess.CompletedProcess(command, 0, text, "")
        self.assertEqual(kwargs["env"]["PYTHONDONTWRITEBYTECODE"], "1")
        stage = Path(command[1]).name if command[0] != "cargo" else "fetch"
        if self.interrupt:
            raise KeyboardInterrupt
        stdout, stderr = kwargs["stdout"], kwargs["stderr"]
        stderr.write("synthetic raw stderr for " + stage + "\n")
        if stage == self.failed_stage:
            stdout.write('{"status":"blocked"}\n')
            return subprocess.CompletedProcess(command, 1)
        if stage == "build_native.py":
            self.make_native()
        elif stage == "fetch":
            self.assertIn("--locked", command)
            self.assertEqual(self.original_lock.read_text(), self.initial_lock_text)
            self.assertEqual(self.native_provenance.read_bytes(), self.initial_provenance)
            staging = Path(command[-1]).parent
            self.assertEqual((staging / "Cargo.lock").read_bytes(), self.lock.read_bytes())
            self.assertEqual((staging / "src/main.rs").read_text(), "fn main() {}\n")
            self.fetch_seen = True
        elif stage == "rebuild_native_locked.py":
            self.original_lock.write_bytes(self.lock.read_bytes())
            provenance = json.loads(self.native_provenance.read_text())
            provenance["lockfile_sha256"] = "f" * 64 if self.corrupt_rebuilt_lock else file_digest(self.lock)
            self.native_provenance.write_text(json.dumps(provenance))
        elif stage == "run.py":
            self.assertEqual(command[2], "paired")
            self.assertEqual(command[command.index("--chromium") + 1], str(self.browser))
            Path(command[command.index("--output") + 1]).write_text('{"synthetic_raw_evidence":true}\n')
        elif stage == "validate.py":
            stdout.write(json.dumps({"status": self.status, "scenarios": []}) + "\n")
            return subprocess.CompletedProcess(command, 0 if self.status == "pass" else 1)
        else:
            self.fail("Unexpected execution subprocess: " + repr(command))
        stdout.write("synthetic raw stdout for " + stage + "\n")
        return subprocess.CompletedProcess(command, 0 if stage != "run.py" or self.status == "pass" else 1)

    def make_native(self):
        native = self.output / "native"
        project = native / "compiler-temp-synthetic/w3cos-build"
        (project / "src").mkdir(parents=True)
        (project / "src/main.rs").write_text("esm_bundle::run_entry_async")
        (project / "src/esm_bundle.rs").write_text("w3cos_runtime::jsdom")
        (project / "Cargo.toml").write_text(
            '[package]\nname = "w3cos-app"\nversion = "0.1.0"\nedition = "2024"\n'
            '[dependencies]\n'
            f'w3cos-core = {{ path = "{self.repo}/crates/w3cos-core" }}\n'
            f'w3cos-runtime = {{ path = "{self.repo}/crates/w3cos-runtime", default-features = false, features = ["gpu"] }}\n'
            f'w3cos-std = {{ path = "{self.repo}/crates/w3cos-std" }}\n'
            '[profile.release]\nopt-level = "z"\nlto = "fat"\ncodegen-units = 1\n'
            'panic = "unwind"\nstrip = "symbols"\n')
        self.original_lock = project / "Cargo.lock"
        self.initial_lock_text = self.lock.read_text().replace("1.0.26", "1.0.27")
        self.original_lock.write_text(self.initial_lock_text)
        binary = native / "w3cos-dom-api-bench"
        binary.write_text("synthetic executable, never launched")
        provenance = {
            "schema": "w3cos-perf-native-build-v1", "route": "esm-aot-headless-release",
            "fixture_sha256": file_digest(self.here / "scenarios.ts"), "binary_sha256": file_digest(binary),
            "compiler_version": "rustc synthetic", "runtime_git_revision": "a" * 40,
            "runtime_diff_sha256": "b" * 64, "source_snapshot_sha256": "c" * 64,
            "cli_sha256": file_digest(self.cli), "generated_esm_sha256": file_digest(project / "src/esm_bundle.rs"),
            "generated_main_sha256": file_digest(project / "src/main.rs"), "lockfile_sha256": file_digest(self.original_lock),
            "build_command": [str(self.cli), "build", str(self.here / "scenarios.ts"), "--release", "--output", str(binary)]}
        self.native_provenance = native / "build-provenance.json"
        self.native_provenance.write_text(json.dumps(provenance))
        self.initial_provenance = self.native_provenance.read_bytes()

    def run_local(self, *extra):
        args = self.args(*extra)
        report, context = local.preflight(args)
        self.assertEqual(report["status"], "ready", report)
        return local.execute(report, context, args.offline)

    def test_preflight_only_checks_prerequisites_without_build_launch_or_fetch(self):
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            status = local.main(["--preflight-only", "--chromium", str(self.browser), "--output-directory", str(self.output)])
        self.assertEqual(status, 0)
        self.assertEqual(json.loads(stdout.getvalue())["status"], "ready")
        self.assertFalse(self.output.exists())
        self.assertTrue(all(call[1].get("capture_output") for call in self.calls))
        self.assertFalse(any(str(self.browser) in call[0] for call in self.calls))
        self.assertTrue(any(call[0][0] == "xcode-select" for call in self.calls))

    def test_old_python_fails_with_clear_version_error(self):
        with patch("run_local.sys.version_info", (3, 9)):
            report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        self.assertIn("3.11+", report["checks"][0]["detail"])
        self.assertEqual(self.calls, [])

    def test_missing_prerequisite_is_readable_and_does_not_build(self):
        for prerequisite in ["cargo --version", "rustc --version", "playwright.sync_api", "xcode-select"]:
            with self.subTest(prerequisite=prerequisite):
                self.probe_failure = prerequisite
                report, _ = local.preflight(self.args())
                self.assertEqual(report["status"], "blocked")
                self.assertIn("synthetic missing prerequisite", str(report))
        self.assertFalse(self.output.exists())

    def test_missing_cli_does_not_fall_back_to_unrelated_path_binary(self):
        self.cli.unlink()
        report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        self.assertIn("Release w3cos CLI is missing", str(report))

    def test_invalid_manifest_and_missing_lock_fail_preflight(self):
        manifest = json.loads((self.here / "manifest.json").read_text())
        manifest["pairs"] = 1
        (self.here / "manifest.json").write_text(json.dumps(manifest))
        self.lock.unlink()
        report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        blocked = {item["name"] for item in report["checks"] if item["status"] == "blocked"}
        self.assertTrue({"pinned_fixture", "pinned_native_lock"} <= blocked)

    def test_rejects_unsupported_build_environment_before_build(self):
        with patch.dict(os.environ, {"RUSTFLAGS": "-C opt-level=0"}):
            report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        self.assertIn("Unrecorded compiler/profile/target override", str(report))
        self.assertFalse(self.output.exists())

    def test_rejects_cargo_configuration_before_build(self):
        cargo_home = Path(os.environ["CARGO_HOME"])
        cargo_home.mkdir()
        (cargo_home / "config.toml").write_text("[build]\n")
        report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        self.assertIn("Unrecorded Cargo configuration", str(report))

    def test_rejects_repository_output_and_existing_output(self):
        report, _ = local.preflight(self.args("--output-directory", str(self.repo / "target/perf-paired/ignored")))
        self.assertEqual(report["status"], "blocked")
        self.assertIn("outside the repository", str(report))
        self.output.mkdir(parents=True)
        marker = self.output / "keep.json"
        marker.write_text("earlier evidence")
        report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        self.assertEqual(marker.read_text(), "earlier evidence")

    def test_rejects_output_under_another_cargo_workspace(self):
        (self.root / "Cargo.toml").write_text("[workspace]\nmembers = []\n")
        report, _ = local.preflight(self.args())
        self.assertEqual(report["status"], "blocked")
        self.assertIn("outside every Cargo workspace", str(report))
        self.assertFalse(self.output.exists())

    def test_default_paths_are_release_cli_native_pin_and_unique_external_output(self):
        first, second = local.arguments([]), local.arguments([])
        self.assertEqual(first.w3cos, self.cli)
        self.assertEqual(first.lockfile, self.lock)
        self.assertNotEqual(first.output_directory, second.output_directory)
        self.assertTrue(first.output_directory.is_relative_to(self.root / "repo-perf-results"))

    def test_discovers_system_chrome_before_other_installed_browsers(self):
        expected = Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
        with patch("run_local.executable", return_value=True), patch("run_local.shutil.which", return_value=None):
            self.assertEqual(local.find_chromium(), expected)

    def test_discovers_home_chromium_and_path_browser(self):
        expected = self.root / "Applications/Chromium.app/Contents/MacOS/Chromium"
        with patch("run_local.Path.home", return_value=self.root), patch("run_local.executable", side_effect=lambda path: path == expected), patch("run_local.shutil.which", return_value=None):
            self.assertEqual(local.find_chromium(), expected)
        expected = Path("/usr/local/bin/chromium")
        with patch("run_local.executable", side_effect=lambda path: path == expected), patch("run_local.shutil.which", side_effect=lambda name: str(expected) if name == "chromium" else None):
            self.assertEqual(local.find_chromium(), expected)

    def test_invalid_explicit_browser_never_falls_back(self):
        with patch("run_local.shutil.which") as which:
            with self.assertRaisesRegex(ValueError, "missing or not executable"):
                local.find_chromium(self.root / "missing")
        which.assert_not_called()

    def test_full_run_preserves_logs_raw_samples_and_fetch_pin(self):
        report = self.run_local()
        self.assertEqual(report["status"], "pass", report)
        self.assertEqual([stage["name"] for stage in report["stages"]],
                         ["build", "fetch-pinned-dependencies", "locked-rebuild", "collect", "validate"])
        self.assertTrue(self.fetch_seen)
        self.assertTrue((self.output / "paired-evidence.json").is_file())
        self.assertEqual(json.loads((self.output / "comparison.json").read_text())["status"], "pass")
        for stage in report["stages"]:
            self.assertTrue(Path(stage["stdout"]).is_file())
            self.assertIn("synthetic raw stderr", Path(stage["stderr"]).read_text())
        self.assertEqual(json.loads((self.output / "local-run.json").read_text()), report)
        self.assertFalse(any("--no-sandbox" in command for command, _ in self.calls))

    def test_offline_uses_no_fetch_and_sets_cargo_offline_for_all_children(self):
        report = self.run_local("--offline")
        self.assertEqual(report["status"], "pass", report)
        self.assertFalse(self.fetch_seen)
        self.assertTrue(all(kwargs["env"]["CARGO_NET_OFFLINE"] == "true" for _, kwargs in self.calls if "env" in kwargs))

    def test_nonpassing_collection_still_runs_independent_validation(self):
        for status in ["fail", "inconclusive", "blocked"]:
            with self.subTest(status=status):
                self.status = status
                self.output = self.root / ("repo-perf-results/local-" + status)
                report = self.run_local()
                self.assertEqual(report["status"], status)
                self.assertEqual(report["stages"][-1]["name"], "validate")
                self.assertEqual(report["stages"][-2]["returncode"], 1)

    def test_failed_build_or_fetch_or_rebuild_never_collects(self):
        for failed in ["build_native.py", "fetch", "rebuild_native_locked.py"]:
            with self.subTest(stage=failed):
                self.failed_stage = failed
                self.output = self.root / ("repo-perf-results/failed-" + failed)
                report = self.run_local()
                self.assertEqual(report["status"], "blocked")
                self.assertNotIn("collect", [stage["name"] for stage in report["stages"]])
                self.assertTrue((self.output / "local-run.json").is_file())

    def test_failed_collector_cannot_be_overridden_by_passing_validator(self):
        self.failed_stage = "run.py"
        report = self.run_local()
        self.assertEqual(report["status"], "blocked")
        self.assertIn("subprocess failed", report["reason"])
        self.assertEqual(report["stages"][-1]["name"], "validate")

    def test_rebuilt_pin_mismatch_never_collects(self):
        self.corrupt_rebuilt_lock = True
        report = self.run_local()
        self.assertEqual(report["status"], "blocked")
        self.assertIn("differs from the selected pinned lock", report["reason"])
        self.assertNotIn("collect", [stage["name"] for stage in report["stages"]])

    def test_pins_changed_after_preflight_fail_closed(self):
        preflight, context = local.preflight(self.args())
        self.lock.write_text(self.lock.read_text().replace("1.0.26", "1.0.25"))
        report = local.execute(preflight, context, False)
        self.assertEqual(report["status"], "blocked")
        self.assertIn("Pinned lockfile changed", report["reason"])
        self.assertEqual(report["stages"], [])

    def test_interruption_retains_incomplete_logs_and_blocked_report(self):
        self.interrupt = True
        report = self.run_local()
        self.assertEqual(report["status"], "blocked")
        self.assertIn("Interrupted", report["reason"])
        self.assertTrue((self.output / "build.stdout.log").is_file())
        self.assertEqual(json.loads((self.output / "local-run.json").read_text())["status"], "blocked")

    def test_main_nonpass_exit_is_nonzero(self):
        self.status = "inconclusive"
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            status = local.main(["--chromium", str(self.browser), "--output-directory", str(self.output)])
        self.assertEqual(status, 1)
        self.assertEqual(json.loads(stdout.getvalue())["status"], "inconclusive")


if __name__ == "__main__":
    unittest.main()
