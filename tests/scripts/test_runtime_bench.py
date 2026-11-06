"""Synthetic validation tests, not real runtime benchmark measurements."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/check-runtime-bench.py"
spec = importlib.util.spec_from_file_location("check_runtime_bench", SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def synthetic_report():
    report = {
        "style_layout": {"nodes": 8201, "median_us": 100},
        "paint_scroll": {"rows": 240, "first_us": 10, "checksum": 123},
        "host_calls": {"median_ns_per_call": 1},
        "startup": {"wall_us": 10},
        "incremental_background": {"median_us": 1, "sibling_moved": False},
    }
    for name in ("width", "text", "class", "insert", "remove"):
        edit = {"median_us": 1, "kind": "style-subtree", "solved_nodes": 2}
        if name in ("insert", "remove"):
            edit.update(kind="splice", children_before=20,
                        children_after=21 if name == "insert" else 19)
        else:
            dimension = "height" if name == "text" else "width"
            edit.update({f"before_{dimension}": 10, f"after_{dimension}": 20})
        report[f"incremental_{name}"] = edit
    return report


class RuntimeBenchTests(unittest.TestCase):
    def test_valid_report_and_extra_harness_fields(self):
        report = synthetic_report()
        report["harness"] = "w3cos-runtime-bench"
        report["style_layout"]["samples_us"] = [99, 100, 101]
        gate.validate_report(report)

    def test_bad_timing_values_in_every_section(self):
        fields = [("style_layout", "median_us"), ("paint_scroll", "first_us"),
                  ("host_calls", "median_ns_per_call"), ("startup", "wall_us")]
        fields += [("incremental_" + name, "median_us") for name in
                   ("width", "background", "text", "class", "insert", "remove")]
        for section, field in fields:
            for value in (True, False, "1", None, 0, -1, float("inf"), float("nan")):
                with self.subTest(section=section, value=value):
                    report = synthetic_report()
                    report[section][field] = value
                    with self.assertRaises(gate.InvalidReport):
                        gate.validate_report(report)

    def test_counters_reject_booleans_negative_and_fractional_values(self):
        fields = [("style_layout", "nodes"), ("paint_scroll", "rows"),
                  ("paint_scroll", "checksum")]
        fields += [("incremental_" + name, "solved_nodes") for name in
                   ("width", "text", "class", "insert", "remove")]
        fields += [("incremental_" + name, field) for name in ("insert", "remove")
                   for field in ("children_before", "children_after")]
        for section, field in fields:
            for value in (True, -1, 1.5, "2", None):
                with self.subTest(section=section, field=field, value=value):
                    report = synthetic_report()
                    report[section][field] = value
                    with self.assertRaises(gate.InvalidReport):
                        gate.validate_report(report)

    def test_dimensions_must_be_finite_nonnegative_and_changed(self):
        for name, dimension in (("width", "width"), ("class", "width"), ("text", "height")):
            for prefix in ("before", "after"):
                for value in (True, "2", None, -1, float("nan"), float("inf")):
                    with self.subTest(name=name, prefix=prefix, value=value):
                        report = synthetic_report()
                        report["incremental_" + name][prefix + "_" + dimension] = value
                        with self.assertRaises(gate.InvalidReport):
                            gate.validate_report(report)
            report = synthetic_report()
            report["incremental_" + name]["after_" + dimension] = 10
            with self.assertRaises(gate.InvalidReport):
                gate.validate_report(report)

    def test_relative_timing_and_subtree_boundaries(self):
        for field, value in (("median_us", 400), ("solved_nodes", 2051), ("kind", "rebuild")):
            report = synthetic_report()
            report["incremental_width"][field] = value
            with self.assertRaises(gate.InvalidReport):
                gate.validate_report(report)
        report = synthetic_report()
        report["incremental_width"].update(median_us=399, solved_nodes=2050)
        gate.validate_report(report)
        # Zero solved nodes is allowed: removal may only shift retained boxes.
        report["incremental_remove"]["solved_nodes"] = 0
        gate.validate_report(report)

    def test_background_requires_explicit_unchanged_sibling(self):
        for value in (True, 0, None, "false"):
            report = synthetic_report()
            report["incremental_background"]["sibling_moved"] = value
            with self.assertRaises(gate.InvalidReport):
                gate.validate_report(report)

    def test_existing_size_checksum_and_mutation_gates_remain(self):
        for section, field, value in (("style_layout", "nodes", 4999),
                                      ("paint_scroll", "rows", 199),
                                      ("paint_scroll", "checksum", 0),
                                      ("incremental_insert", "children_after", 20),
                                      ("incremental_remove", "children_after", 20)):
            report = synthetic_report()
            report[section][field] = value
            with self.assertRaises(gate.InvalidReport):
                gate.validate_report(report)

    def test_missing_malformed_sections_and_fields(self):
        for name, section in synthetic_report().items():
            for replacement in (None, [], "value", 1):
                report = synthetic_report()
                report[name] = replacement
                with self.assertRaises(gate.InvalidReport):
                    gate.validate_report(report)
            for field in section:
                report = synthetic_report()
                del report[name][field]
                with self.assertRaises(gate.InvalidReport):
                    gate.validate_report(report)
        for report in (None, [], 1):
            with self.assertRaises(gate.InvalidReport):
                gate.validate_report(report)

    def test_json_rejects_duplicates_nonfinite_and_overflow(self):
        for text in ('{"x":1,"x":2}', '{"x":NaN}', '{"x":Infinity}'):
            with self.assertRaises(gate.InvalidReport):
                gate.load_report(text)
        text = json.dumps(synthetic_report()).replace('"wall_us": 10', '"wall_us": 1e999')
        with self.assertRaises(gate.InvalidReport):
            gate.validate_report(gate.load_report(text))

    def test_cli_normal_and_optimized_python_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            for optimize in ([], ["-O"]):
                for payload, expected in ((json.dumps(synthetic_report()), 0),
                                          ('{"style_layout":null}', 1), ('not json', 1)):
                    path.write_text(payload)
                    result = subprocess.run([sys.executable, *optimize, str(SCRIPT), str(path)],
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stderr)
                    self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
