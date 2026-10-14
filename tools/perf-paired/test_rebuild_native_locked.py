"""Synthetic filesystem/subprocess tests; never build or execute project code."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import rebuild_native_locked as rebuild
from run import verify_native
from validate import digest, file_digest, load_json, validate_native_configuration


class LockedRebuildTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.repo = self.root / "checkout"
        self.repo.mkdir()
        self.output = self.root / "retained-build"
        self.project = self.output / "compiler-temp-original/w3cos-build"
        (self.project / "src").mkdir(parents=True)
        (self.project / "target/release").mkdir(parents=True)
        tools = self.repo / "tools/perf-paired"
        tools.mkdir(parents=True)
        tools.joinpath("manifest.json").write_bytes(Path(__file__).with_name("manifest.json").read_bytes())
        self.fixture = tools / "scenarios.ts"
        self.fixture.write_bytes(b"synthetic fixture, never executed")
        self.source = self.repo / "runtime.rs"
        self.source.write_bytes(b"synthetic runtime, never compiled")
        self.cli, self.cargo = self.root / "cli", self.root / "cargo"
        self.cli.write_bytes(b"synthetic CLI, never executed")
        self.cargo.write_bytes(b"synthetic Cargo, never executed")
        self.binary = self.output / "w3cos-dom-api-bench"
        self.binary.write_bytes(b"original synthetic executable")
        self.binary.chmod(0o755)
        self.built = self.project / "target/release/w3cos-app"
        self.built.write_bytes(self.binary.read_bytes())
        self.built.chmod(0o755)
        (self.project / "src/main.rs").write_text("esm_bundle::run_entry_async")
        (self.project / "src/esm_bundle.rs").write_text("w3cos_runtime::jsdom")
        self.generated_manifest = self.project / "Cargo.toml"
        self.generated_manifest.write_text(self.manifest_text())
        self.lock = self.project / "Cargo.lock"
        self.lock.write_text('version = 4\n[[package]]\nname = "synthetic"\nversion = "1.0.27"\n')
        self.pinned = self.root / "baseline-Cargo.lock"
        self.pinned.write_text(self.lock.read_text().replace("1.0.27", "1.0.26"))
        self.revision, self.compiler_version = "a" * 40, "rustc synthetic"
        self.patch_bytes = b"synthetic tracked source patch"
        self.snapshot_path = self.output / "source-snapshot.json"
        self.snapshot_path.write_text(json.dumps(self.snapshot()))
        self.patch_path = self.output / "runtime-source.patch"
        self.patch_path.write_bytes(self.patch_bytes)
        self.provenance_path = self.output / "build-provenance.json"
        self.original = {
            "schema": "w3cos-perf-native-build-v1", "route": "esm-aot-headless-release",
            "fixture_sha256": file_digest(self.fixture), "binary_sha256": file_digest(self.binary),
            "compiler_version": self.compiler_version, "runtime_git_revision": self.revision,
            "runtime_diff_sha256": hashlib.sha256(self.patch_bytes).hexdigest(),
            "source_snapshot_sha256": digest(self.snapshot()), "cli_sha256": file_digest(self.cli),
            "generated_esm_sha256": file_digest(self.project / "src/esm_bundle.rs"),
            "generated_main_sha256": file_digest(self.project / "src/main.rs"),
            "lockfile_sha256": file_digest(self.lock),
            "build_command": [str(self.cli), "build", str(self.fixture), "--release", "--output", str(self.binary)]}
        self.provenance_path.write_text(json.dumps(self.original, indent=2) + "\n")
        self.original_provenance_bytes = self.provenance_path.read_bytes()
        self.output.joinpath("build.log").write_text("original CLI build log")
        self.args = argparse.Namespace(build_directory=self.output, repo_root=self.repo,
                                       lockfile=self.pinned, cargo=self.cargo)
        self.build_count = 0
        # Every subprocess is mocked, including version/Git probes. A synthetic
        # Cargo executable cannot run even if a test accidentally stops mocking.
        for target, kwargs in [
            ("rebuild_native_locked.source_snapshot", {"side_effect": lambda repo: self.snapshot()}),
            ("rebuild_native_locked.subprocess.check_output", {"side_effect": self.check_output}),
            ("rebuild_native_locked.subprocess.run", {"side_effect": self.build}),
        ]:
            mock = patch(target, **kwargs)
            value = mock.start()
            self.addCleanup(mock.stop)
            if target.endswith("subprocess.run"):
                self.run = value
        environment = patch.dict(os.environ, {"PATH": str(self.root), "CARGO_HOME": str(self.root / "cargo-home")}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)

    def manifest_text(self):
        return ('[package]\nname = "w3cos-app"\nversion = "0.1.0"\nedition = "2024"\n'
                '[dependencies]\n'
                f'w3cos-core = {{ path = "{self.repo}/crates/w3cos-core" }}\n'
                f'w3cos-runtime = {{ path = "{self.repo}/crates/w3cos-runtime", default-features = false, features = ["gpu"] }}\n'
                f'w3cos-std = {{ path = "{self.repo}/crates/w3cos-std" }}\n'
                '[profile.release]\nopt-level = "z"\nlto = "fat"\ncodegen-units = 1\npanic = "unwind"\nstrip = "symbols"\n')

    def snapshot(self):
        return {str(path.relative_to(self.repo)): file_digest(path) for path in self.repo.rglob("*") if path.is_file()}

    def check_output(self, command, **kwargs):
        if command[0] == "git":
            return self.revision + "\n" if "rev-parse" in command else self.patch_bytes
        self.assertEqual(kwargs["cwd"], self.project)
        if command[0] == "rustc":
            return self.compiler_version + "\n"
        self.assertEqual(command, [str(self.cargo), "--version"])
        return "cargo synthetic\n"

    def build(self, command, **kwargs):
        self.build_count += 1
        self.assertEqual(command, [str(self.cargo), "build", "--release", "--locked", "--offline"])
        self.assertEqual(kwargs["cwd"], self.project)
        self.assertNotIn("CARGO_BUILD_TARGET", kwargs["env"])
        self.assertEqual(kwargs["env"]["CARGO_TARGET_DIR"], str(self.project / "target"))
        self.assertEqual(kwargs["env"]["CARGO_NET_OFFLINE"], "true")
        self.assertEqual(self.lock.read_bytes(), self.pinned.read_bytes())
        self.assertEqual(load_json(self.provenance_path.read_text())["status"], "locked-rebuild-incomplete")
        self.assertFalse(self.built.exists(), "Stale target executable must be archived before Cargo")
        kwargs["stdout"].write("synthetic Cargo diagnostics\n")
        self.built.write_bytes(f"synthetic rebuilt executable {self.build_count}".encode())
        self.built.chmod(0o755)
        return subprocess.CompletedProcess(command, 0)

    def final_provenance(self):
        return load_json(self.provenance_path.read_text())

    def assert_incomplete(self):
        self.assertEqual(self.final_provenance()["status"], "locked-rebuild-incomplete")
        with self.assertRaises(ValueError):
            verify_native(self.binary, self.provenance_path, self.original["fixture_sha256"])
        self.assertFalse((self.output / ".locked-rebuild-in-progress").exists())

    def test_success_pins_exact_lock_and_preserves_original_history(self):
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": "wrong-target", "CARGO_BUILD_TARGET": "wrong-triple"}):
            report = rebuild.rebuild(self.args)
        final = verify_native(self.binary, self.provenance_path, self.original["fixture_sha256"])
        self.assertEqual(report["status"], "rebuilt")
        self.assertEqual(final["lockfile_sha256"], file_digest(self.pinned))
        self.assertEqual(final["build_command"], self.original["build_command"])
        metadata = final["locked_rebuild"]
        self.assertEqual(metadata["previous_provenance"], self.original)
        history = Path(report["history_directory"])
        self.assertEqual(history.joinpath("build-provenance.json").read_bytes(), self.original_provenance_bytes)
        self.assertEqual(file_digest(history / "Cargo.lock"), self.original["lockfile_sha256"])
        self.assertEqual(file_digest(history / self.binary.name), self.original["binary_sha256"])
        self.assertEqual(history.joinpath("pinned-Cargo.lock").read_bytes(), self.pinned.read_bytes())
        self.assertEqual(self.output.joinpath("build.log").read_text(), "original CLI build log")
        self.assertEqual(load_json(history.joinpath("result-provenance.json").read_text()), final)
        self.assertEqual(metadata["build_log_sha256"], file_digest(history / "rebuild.log"))

    def test_second_successful_rebuild_retains_complete_provenance_chain(self):
        first_report = rebuild.rebuild(self.args)
        first = self.final_provenance()
        second_report = rebuild.rebuild(self.args)
        second = self.final_provenance()
        validate_native_configuration(second, self.original["fixture_sha256"])
        self.assertNotEqual(first_report["history_directory"], second_report["history_directory"])
        self.assertEqual(second["locked_rebuild"]["previous_provenance"], first)
        self.assertEqual(first["locked_rebuild"]["previous_provenance"], self.original)
        self.assertEqual(len(list(self.output.joinpath("locked-rebuild-history").iterdir())), 2)

    def test_preflight_rejects_source_generated_lock_and_binary_drift(self):
        for path in [self.source, self.fixture, self.cli, self.binary, self.lock,
                     self.project / "src/main.rs", self.project / "src/esm_bundle.rs", self.snapshot_path, self.patch_path]:
            original = path.read_bytes()
            with self.subTest(path=path):
                path.write_bytes(original + b"\nchanged")
                with self.assertRaises(ValueError):
                    rebuild.rebuild(self.args)
                self.run.assert_not_called()
                self.assertEqual(self.provenance_path.read_bytes(), self.original_provenance_bytes)
                path.write_bytes(original)

    def test_generated_manifest_dependency_profile_and_extra_inputs_rejected(self):
        original = self.generated_manifest.read_text()
        for content in [original.replace(str(self.repo), str(self.root / "different-checkout")),
                        original.replace('lto = "fat"', 'lto = "off"'),
                        original + '\n[build-dependencies]\nextra = "1"\n',
                        original.replace("codegen-units = 1", "codegen-units = true")]:
            with self.subTest(content=content):
                self.generated_manifest.write_text(content)
                with self.assertRaises(ValueError): rebuild.rebuild(self.args)
                self.run.assert_not_called()
        self.generated_manifest.write_text(original)
        for relative in ["build.rs", "src/extra.rs", "src/bin/extra.rs", ".cargo/config.toml", "rust-toolchain.toml"]:
            path = self.project / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("synthetic unrecorded input")
            with self.subTest(relative=relative), self.assertRaises(ValueError): rebuild.rebuild(self.args)
            path.unlink()
            if path.parent not in (self.project, self.project / "src"):
                path.parent.rmdir()
        self.run.assert_not_called()

    def test_rejects_wrong_checkout_revision_compiler_and_duplicate_project(self):
        for attribute, replacement in [("revision", "b" * 40), ("compiler_version", "rustc changed")]:
            old = getattr(self, attribute)
            setattr(self, attribute, replacement)
            with self.subTest(attribute=attribute), self.assertRaises(ValueError): rebuild.rebuild(self.args)
            setattr(self, attribute, old)
        wrong = copy.copy(self.args)
        wrong.repo_root = self.root / "wrong-checkout"
        wrong.repo_root.mkdir()
        with self.assertRaises(ValueError): rebuild.rebuild(wrong)
        import shutil
        shutil.copytree(self.project, self.output / "compiler-temp-copy/w3cos-build")
        with self.assertRaises(ValueError): rebuild.rebuild(self.args)
        self.run.assert_not_called()

    def test_rejects_symlink_and_environment_configuration_overrides(self):
        source = self.root / "aliased-lock"
        source.write_bytes(self.lock.read_bytes())
        self.lock.unlink()
        self.lock.symlink_to(source)
        with self.assertRaises(ValueError): rebuild.rebuild(self.args)
        self.lock.unlink()
        self.lock.write_bytes(source.read_bytes())
        for key in ["RUSTFLAGS", "RUSTC", "RUSTC_WRAPPER", "CARGO_PROFILE_RELEASE_LTO", "CARGO_TARGET_X86_64_RUSTFLAGS"]:
            with self.subTest(key=key), patch.dict(os.environ, {key: "override"}), self.assertRaises(ValueError):
                rebuild.rebuild(self.args)
        home = self.root / "cargo-home"
        home.mkdir()
        home.joinpath("config.toml").write_text("synthetic config")
        with self.assertRaises(ValueError): rebuild.rebuild(self.args)
        self.run.assert_not_called()

    def test_rejects_symlinked_empty_target_directory_before_cargo(self):
        self.built.unlink()
        self.built.parent.rmdir()
        outside = self.root / "outside-target"
        outside.mkdir()
        self.built.parent.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError): rebuild.rebuild(self.args)
        self.run.assert_not_called()
        self.assertEqual(list(outside.iterdir()), [])
        self.assertEqual(self.provenance_path.read_bytes(), self.original_provenance_bytes)

    def test_nonzero_timeout_and_missing_output_keep_failure_and_history(self):
        for failure in ["nonzero", "timeout", "missing-output"]:
            # A failed mutation is terminal for this retained success; restore
            # explicitly from the archived history to exercise the next case.
            def failed(command, **kwargs):
                kwargs["stdout"].write("partial synthetic failure diagnostics\n")
                if failure == "timeout":
                    raise subprocess.TimeoutExpired(command, 1800)
                return subprocess.CompletedProcess(command, 1 if failure == "nonzero" else 0)
            self.run.side_effect = failed
            with self.subTest(failure=failure), self.assertRaises((ValueError, subprocess.TimeoutExpired)):
                rebuild.rebuild(self.args)
            self.assert_incomplete()
            history = Path(self.final_provenance()["history_directory"])
            self.assertIn("partial synthetic", history.joinpath("rebuild.log").read_text())
            self.assertTrue(history.joinpath("error.json").is_file())
            self.assertEqual(file_digest(history / self.binary.name), self.original["binary_sha256"])
            self.provenance_path.write_bytes(history.joinpath("build-provenance.json").read_bytes())
            self.lock.write_bytes(history.joinpath("Cargo.lock").read_bytes())
        self.run.side_effect = self.build
        rebuild.rebuild(self.args)
        validate_native_configuration(self.final_provenance(), self.original["fixture_sha256"])

    def test_mutation_during_cargo_never_publishes_success(self):
        for path in [self.source, self.fixture, self.cli, self.cargo, self.lock, self.pinned,
                     self.binary, self.generated_manifest, self.project / "src/main.rs", self.snapshot_path,
                     self.patch_path, self.provenance_path]:
            old = path.read_bytes()
            lock_bytes = self.lock.read_bytes()
            def changed(command, **kwargs):
                result = self.build(command, **kwargs)
                path.write_bytes(path.read_bytes() + b"\nchanged")
                return result
            self.run.side_effect = changed
            with self.subTest(path=path), self.assertRaises(ValueError): rebuild.rebuild(self.args)
            # The only malformed active marker is the one deliberately changed
            # by the fake external writer; neither may become valid evidence.
            with self.assertRaises(ValueError):
                verify_native(self.binary, self.provenance_path, self.original["fixture_sha256"])
            path.write_bytes(old)
            self.lock.write_bytes(lock_bytes)
            self.provenance_path.write_bytes(self.original_provenance_bytes)

    def test_optional_metadata_is_strict_and_legacy_provenance_remains_valid(self):
        validate_native_configuration(self.original, self.original["fixture_sha256"])
        rebuild.rebuild(self.args)
        valid = self.final_provenance()
        malformed = [None, [], {}, {**valid["locked_rebuild"], "unexpected": True}]
        for metadata in malformed:
            value = {**valid, "locked_rebuild": metadata}
            with self.subTest(metadata=metadata), self.assertRaises(ValueError):
                validate_native_configuration(value, self.original["fixture_sha256"])
        for field in valid["locked_rebuild"]:
            value = copy.deepcopy(valid)
            del value["locked_rebuild"][field]
            with self.subTest(missing=field), self.assertRaises(ValueError):
                validate_native_configuration(value, self.original["fixture_sha256"])
        for field, bad in [("schema", "unknown"), ("previous_provenance_sha256", "b" * 64),
                           ("binary_sha256", "b" * 64), ("lockfile_sha256", "b" * 64),
                           ("command", [str(self.cargo), "build", "--release"]),
                           ("working_directory", "relative"), ("generated_inputs", {}),
                           ("cargo_version", ""), ("build_log_sha256", True)]:
            value = copy.deepcopy(valid)
            value["locked_rebuild"][field] = bad
            with self.subTest(field=field), self.assertRaises(ValueError):
                validate_native_configuration(value, self.original["fixture_sha256"])
        for value in [self.original, valid]:
            with self.assertRaises(ValueError):
                validate_native_configuration({**value, "status": "build-incomplete"}, self.original["fixture_sha256"])

    def test_changed_original_identity_and_nested_incomplete_history_rejected(self):
        rebuild.rebuild(self.args)
        valid = self.final_provenance()
        value = copy.deepcopy(valid)
        value["runtime_git_revision"] = "b" * 40
        with self.assertRaises(ValueError): validate_native_configuration(value, self.original["fixture_sha256"])
        value = copy.deepcopy(valid)
        previous = value["locked_rebuild"]["previous_provenance"]
        previous["status"] = "build-incomplete"
        value["locked_rebuild"]["previous_provenance_sha256"] = digest(previous)
        with self.assertRaises(ValueError): validate_native_configuration(value, self.original["fixture_sha256"])


if __name__ == "__main__":
    unittest.main()
