"""Synthetic schema tests only. Their generated durations are never benchmark evidence."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import contextlib
import io
import os
import subprocess
import build_native

from run import parse_result, verify_native
from validate import CASES, PARAMETERS, VIEWPORT, METRIC, RESET, compare, digest, file_digest, load_manifest, load_json

MANIFEST = load_manifest(Path(__file__).with_name("manifest.json"))
HASH = "a" * 64
BUILD = {"schema": "w3cos-perf-native-build-v1", "route": "esm-aot-headless-release",
         "fixture_sha256": HASH, "binary_sha256": HASH, "lockfile_sha256": HASH,
         "generated_esm_sha256": HASH, "cli_sha256": HASH, "generated_main_sha256": HASH,
         "runtime_diff_sha256": HASH, "source_snapshot_sha256": HASH, "compiler_version": "test", "runtime_git_revision": "a" * 40,
         "build_command": ["synthetic-cli", "build", "synthetic-fixture", "--release", "--output", "synthetic-binary"]}
BROWSER = {"sandbox_enabled": True, "offline": True, "headless": True, "service_workers": "block",
           "ignored_default_args": ["--unsafely-disable-devtools-self-xss-warnings", "--enable-unsafe-swiftshader"],
           "viewport": VIEWPORT, "chromium_version": "synthetic", "playwright_version": "synthetic",
           "executable_sha256": HASH}


def synthetic_result(pair, engine, ratio):
    cases = []
    n, rounds = PARAMETERS["nodes"], PARAMETERS["rounds"]
    for case_index, case in enumerate(CASES):
        evidence = ({"checks": rounds * (5 * n + 2), "final_children": 0} if case_index == 0 else
                    {"checks": 3 * n * rounds, "true_matches": n * rounds,
                     "false_matches": n * rounds, "final_class": "bench inactive"})
        cases.append({"id": case, "samples": [
            {"phase": "measurement", "index": i,
             "elapsed_ms": (100 + pair + case_index + i / 100) * (ratio if engine == "native" else 1),
             "evidence": copy.deepcopy(evidence)} for i in range(PARAMETERS["samples"])]})
    return {"schema": "w3cos-dom-api-v1", "status": "ok", "metric": METRIC, "reset": RESET,
            "parameters": copy.deepcopy(PARAMETERS), "viewport": copy.deepcopy(VIEWPORT), "scenarios": cases}


def synthetic_envelope(ratio=0.7):
    envelope = {"schema": "w3cos-paired-evidence-v1", "collection_status": "complete",
                "manifest_sha256": digest(MANIFEST), "fixture_sha256": HASH,
                "host_session": "unit-test-host-session", "native_build": copy.deepcopy(BUILD), "runs": []}
    for pair in range(8):
        for engine in (["native", "browser"] if pair % 2 == 0 else ["browser", "native"]):
            result = synthetic_result(pair, engine, ratio)
            configuration = copy.deepcopy(BUILD) if engine == "native" else copy.deepcopy(BROWSER)
            envelope["runs"].append({"pair": pair, "order": len(envelope["runs"]), "engine": engine,
                "run_id": f"synthetic-{pair}-{engine}", "host_session": envelope["host_session"],
                "fixture_sha256": HASH, "engine_version": (BUILD["runtime_git_revision"] + "; rustc=" + BUILD["compiler_version"]
                                   if engine == "native" else "synthetic; playwright=synthetic"),
                "execution_mode": "esm-aot-headless-release" if engine == "native" else "chromium-headless",
                "engine_configuration": configuration, "configuration_sha256": digest(configuration),
                "result": result, "result_sha256": digest(result)})
    return envelope


class ValidationTests(unittest.TestCase):
    def assert_blocked(self, envelope):
        self.assertEqual(compare(envelope, MANIFEST, HASH)["status"], "blocked")

    def test_synthetic_fast_slow_and_equal_outcomes(self):
        for ratio, status in [(0.7, "pass"), (1.3, "fail"), (1.0, "inconclusive"), (0.96, "inconclusive")]:
            with self.subTest(ratio=ratio):
                self.assertEqual(compare(synthetic_envelope(ratio), MANIFEST, HASH)["status"], status)

    def test_one_slow_scenario_cannot_hide_in_average(self):
        value = synthetic_envelope()
        for run in value["runs"]:
            if run["engine"] == "native":
                for sample in run["result"]["scenarios"][1]["samples"]:
                    sample["elapsed_ms"] *= 2
                run["result_sha256"] = digest(run["result"])
        result = compare(value, MANIFEST, HASH)
        self.assertEqual(result["status"], "fail")
        self.assertEqual([item["status"] for item in result["scenarios"]], ["pass", "fail"])

    def test_incomplete_and_missing_arms_never_pass(self):
        for state in ["browser-only", "blocked", "incomplete", None]:
            value = synthetic_envelope(); value["collection_status"] = state
            with self.subTest(state=state): self.assert_blocked(value)
        for length in [0, 1, 8, 15]:
            value = synthetic_envelope(); value["runs"] = value["runs"][:length]
            with self.subTest(length=length): self.assert_blocked(value)

    def test_metadata_mismatch_or_reused_run_blocks(self):
        for field, replacement in [("pair", 3), ("pair", False), ("order", 10), ("engine", "browser"),
                                   ("host_session", "another-host"), ("fixture_sha256", "wrong"),
                                   ("execution_mode", "legacy-fake-document"), ("engine_version", ""),
                                   ("result_sha256", "wrong")]:
            value = synthetic_envelope(); value["runs"][0][field] = replacement
            with self.subTest(field=field, replacement=replacement): self.assert_blocked(value)
        value = synthetic_envelope(); value["runs"][1]["run_id"] = value["runs"][0]["run_id"]
        self.assert_blocked(value)
        value = synthetic_envelope(); value["runs"][3]["result"] = value["runs"][0]["result"]
        value["runs"][3]["result_sha256"] = value["runs"][0]["result_sha256"]
        self.assert_blocked(value)
        value = synthetic_envelope(); value["runs"][3]["engine_version"] = "changed"
        self.assert_blocked(value)

    def test_changed_native_build_with_same_revision_is_rejected(self):
        value = synthetic_envelope()
        run = value["runs"][3]
        run["engine_configuration"]["binary_sha256"] = "b" * 64
        run["configuration_sha256"] = digest(run["engine_configuration"])
        self.assert_blocked(value)

    def test_incomplete_native_or_browser_metadata_cannot_pass(self):
        for engine, template in [("native", BUILD), ("browser", BROWSER)]:
            for field in template:
                value = synthetic_envelope()
                if engine == "native": del value["native_build"][field]
                for run in value["runs"]:
                    if run["engine"] == engine:
                        del run["engine_configuration"][field]
                        run["configuration_sha256"] = digest(run["engine_configuration"])
                with self.subTest(engine=engine, field=field): self.assert_blocked(value)

    def test_boolean_configured_dpr_is_rejected(self):
        value = synthetic_envelope()
        for run in value["runs"]:
            if run["engine"] == "browser":
                run["engine_configuration"]["viewport"]["dpr"] = True
                run["configuration_sha256"] = digest(run["engine_configuration"])
        self.assert_blocked(value)

    def test_variable_pairs_and_one_slow_pair_are_not_hidden(self):
        for ratios in [[.92, .94, .96, .98, .91, .99, 1.01, .95], [.6] * 7 + [1.01]]:
            value = synthetic_envelope()
            for run in value["runs"]:
                if run["engine"] == "native":
                    for sample in run["result"]["scenarios"][0]["samples"]:
                        sample["elapsed_ms"] *= ratios[run["pair"]] / .7
                    run["result_sha256"] = digest(run["result"])
            report = compare(value, MANIFEST, HASH)
            self.assertEqual(report["status"], "inconclusive")
            self.assertEqual(report["scenarios"][0]["status"], "inconclusive")
            self.assertEqual(report["scenarios"][1]["status"], "pass")

    def test_result_schema_semantics_parameters_and_viewport(self):
        for field, replacement in [("schema", "unknown"), ("status", "error"), ("metric", "cpu_frame"),
                                   ("reset", "persistent-document"), ("parameters", {}),
                                   ("viewport", {"width": 1200, "height": 800, "dpr": 1})]:
            value = synthetic_envelope(); value["runs"][0]["result"][field] = replacement
            with self.subTest(field=field): self.assert_blocked(value)

    def test_scenario_completeness(self):
        for replacement in [[], [{}], [None, None], [None] + synthetic_result(0, "native", .7)["scenarios"]]:
            value = synthetic_envelope(); value["runs"][0]["result"]["scenarios"] = replacement
            with self.subTest(replacement=replacement): self.assert_blocked(value)
        value = synthetic_envelope(); cases = value["runs"][0]["result"]["scenarios"]
        cases[1]["id"] = cases[0]["id"]; self.assert_blocked(value)

    def test_bad_samples(self):
        for duration in [0, -1, float("nan"), float("inf"), True, "1", None]:
            value = synthetic_envelope(); value["runs"][0]["result"]["scenarios"][0]["samples"][0]["elapsed_ms"] = duration
            with self.subTest(duration=duration): self.assert_blocked(value)
        for field, replacement in [("phase", "warmup"), ("index", 1), ("index", False), ("evidence", {})]:
            value = synthetic_envelope(); value["runs"][0]["result"]["scenarios"][0]["samples"][0][field] = replacement
            with self.subTest(field=field): self.assert_blocked(value)
        value = synthetic_envelope(); value["runs"][0]["result"]["scenarios"][0]["samples"].pop()
        self.assert_blocked(value)

    def test_wrong_correctness_evidence_blocks_even_with_valid_digest(self):
        for case_index, field, bad in [(0, "checks", 0), (0, "final_children", 1),
                                       (1, "true_matches", 0), (1, "false_matches", 0),
                                       (1, "final_class", "bench active")]:
            value = synthetic_envelope(); run = value["runs"][0]
            run["result"]["scenarios"][case_index]["samples"][0]["evidence"][field] = bad
            run["result_sha256"] = digest(run["result"])
            with self.subTest(field=field): self.assert_blocked(value)

    def test_no_vacuous_or_weakened_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            for replacement in [{}, {**MANIFEST, "scenarios": []}, {**MANIFEST, "native_ratio_limit": 1.1}]:
                path.write_text(json.dumps(replacement))
                with self.subTest(replacement=replacement), self.assertRaises(ValueError): load_manifest(path)

    def test_result_parser_requires_exactly_one_record(self):
        payload = '{"status":"ok"}'
        for line in ["W3COS_PERF_RESULT " + payload, "[js.console.log] W3COS_PERF_RESULT " + payload]:
            self.assertEqual(parse_result([line]), {"status": "ok"})
            with self.assertRaises(ValueError): parse_result([line, line])
        for lines in [[], ["W3COS_DOM_OK nodes=123"], ["W3COS_PERF_RESULT {"], ["noise W3COS_PERF_RESULT {}"]]:
            with self.subTest(lines=lines), self.assertRaises(ValueError): parse_result(lines)

    def test_duplicate_json_keys_and_nonstandard_constants_are_rejected(self):
        for text in ['{"status":"error","status":"ok"}', '{"value":NaN}', '{"x":{"y":1,"y":2}}']:
            with self.subTest(text=text), self.assertRaises(ValueError): load_json(text)
        with self.assertRaises(ValueError):
            parse_result(['W3COS_PERF_RESULT {"status":"error","status":"ok"}'])

    def test_failed_rebuild_invalidates_previous_success_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            provenance = output / "build-provenance.json"
            provenance.write_text(json.dumps(BUILD))
            argv = ["build_native.py", "--w3cos", str(output / "missing-cli"),
                    "--output-directory", str(output)]
            with patch("sys.argv", argv), patch("build_native.subprocess.check_output", return_value=b""), \
                    patch("build_native.subprocess.run", side_effect=OSError("synthetic build failure")), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(build_native.main(), 1)
            self.assertEqual(json.loads(provenance.read_text())["status"], "build-incomplete")
            self.assertNotIn("route", json.loads(provenance.read_text()))

    def test_native_provenance_binds_binary_and_exact_source(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "not-executed"; binary.write_bytes(b"synthetic provenance test")
            path = Path(directory) / "build.json"
            valid = {**BUILD, "binary_sha256": file_digest(binary)}
            path.write_text(json.dumps(valid)); verify_native(binary, path, HASH)
            for root in [None, [], False, 1, "invalid"]:
                path.write_text(json.dumps(root))
                with self.subTest(root=root), self.assertRaises(ValueError): verify_native(binary, path, HASH)
            for field, replacement in [("route", "legacy"), ("fixture_sha256", "wrong"), ("binary_sha256", "wrong")]:
                path.write_text(json.dumps({**valid, field: replacement}))
                with self.subTest(field=field), self.assertRaises(ValueError): verify_native(binary, path, HASH)

    def test_native_build_is_host_scoped_and_retains_timeout_log(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            repo = Path(__file__).resolve().parents[2]
            argv = ["build_native.py", "--w3cos", str(output / "mock-cli"),
                    "--output-directory", str(output)]

            def timed_out(command, **kwargs):
                self.assertEqual(kwargs["cwd"], repo)
                self.assertNotIn("CARGO_TARGET_DIR", kwargs["env"])
                self.assertNotIn("CARGO_BUILD_TARGET", kwargs["env"])
                kwargs["stdout"].write("synthetic partial build diagnostics\n")
                raise subprocess.TimeoutExpired(command, 1800)

            with patch("sys.argv", argv), \
                    patch.dict(os.environ, {"CARGO_TARGET_DIR": "elsewhere", "CARGO_BUILD_TARGET": "cross-target"}), \
                    patch("build_native.subprocess.check_output", return_value=b""), \
                    patch("build_native.subprocess.run", side_effect=timed_out), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(build_native.main(), 1)
            self.assertIn("synthetic partial", (output / "build.log").read_text())
            self.assertEqual(json.loads((output / "build-provenance.json").read_text())["status"], "build-incomplete")


if __name__ == "__main__":
    unittest.main()
