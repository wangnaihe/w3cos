"""Synthetic driver tests. Durations come from the fake executable or a probe, never from this file's assertions being treated as a VS Code run."""
import json
import os
import stat
import tempfile
import textwrap
import unittest
from pathlib import Path

import vscode_driver
import vscode_validate
from vscode_validate import derive_status, load_manifest, summarize, validate_envelope


MANIFEST = load_manifest(Path(__file__).with_name("vscode-baseline-manifest.json"))
HERE = Path(__file__).parent


def write_executable(path, body):
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC)


class DriverTests(unittest.TestCase):
    def test_missing_binary_is_blocked_without_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "out.json"
            args = vscode_driver.argparse.Namespace(
                code=str(Path(directory) / "missing"), manifest=HERE / "vscode-baseline-manifest.json",
                output=output, samples=1, memory_delay=0.01, timeout=5, proc_root=Path("/proc"),
                workspace=None, fixture_file=None, window_probe=None, open_probe=None, typing_probe=None,
                code_arg=[])
            status = vscode_driver.collect(args)
            envelope = json.loads(output.read_text())
        self.assertEqual(status, 1)
        self.assertEqual(envelope["collection_status"], "blocked")
        self.assertEqual(envelope["samples"], [])
        self.assertNotIn("elapsed_ms", json.dumps(envelope["samples"]))

    def test_fake_code_records_markers_and_memory_and_leaves_probes_blocked(self):
        with tempfile.TemporaryDirectory() as directory:
            code = Path(directory) / "code"
            write_executable(code, textwrap.dedent("""\
                #!/usr/bin/env python3
                import sys, time
                args = sys.argv
                def value(flag):
                    return args[args.index(flag) + 1]
                open(value("--prof-duration-markers-file"), "w").write(
                    "ellapsed\\t100\\tcode/didStartMain-code/didStartWorkbench\\t80\\n")
                open(value("--prof-append-timers"), "w").write(
                    "80\\tCode - OSS\\te97039dd92\\tsess\\tstandard_start\\t0100ms\\n")
                time.sleep(0.4)
            """))
            output = Path(directory) / "out.json"
            args = vscode_driver.argparse.Namespace(
                code=str(code), manifest=HERE / "vscode-baseline-manifest.json",
                output=output, samples=1, memory_delay=0.05, timeout=5, proc_root=Path("/proc"),
                workspace=None, fixture_file=None, window_probe=None, open_probe=None, typing_probe=None,
                code_arg=[])
            status = vscode_driver.collect(args)
            envelope = json.loads(output.read_text())
        self.assertEqual(status, 0, envelope.get("error"))
        self.assertEqual(envelope["collection_status"], "partial")
        workbench = envelope["samples"][0]["scenarios"]["cold-start-workbench-ready"]
        self.assertEqual(workbench["elapsed_ms"], 80)
        self.assertEqual(workbench["source"], "vscode-prof-duration-markers")
        self.assertTrue(workbench["standard_start"])
        memory = envelope["samples"][0]["scenarios"]["memory-pss"]
        self.assertEqual(memory["status"], "ok")
        self.assertGreater(memory["pss_bytes"], 0)
        self.assertGreaterEqual(memory["process_count"], 1)
        typing = envelope["samples"][0]["scenarios"]["typing-latency"]
        self.assertEqual(typing, {"status": "blocked", "reason": "typing-probe-not-configured"})
        self.assertNotIn("elapsed_ms", typing)
        validate_envelope(envelope, MANIFEST)

    def test_probe_measurement_is_stored_and_self_report_cannot_complete_typing(self):
        with tempfile.TemporaryDirectory() as directory:
            code = Path(directory) / "code"
            probe = Path(directory) / "probe"
            write_executable(code, textwrap.dedent("""\
                #!/usr/bin/env python3
                import sys, time
                args = sys.argv
                def value(flag):
                    return args[args.index(flag) + 1]
                open(value("--prof-duration-markers-file"), "w").write(
                    "code/didStartMain-code/didStartWorkbench\\t90\\n")
                time.sleep(0.3)
            """))
            write_executable(probe, textwrap.dedent("""\
                #!/usr/bin/env python3
                print('{"schema":"w3cos-vscode-probe-v1","source":"compositor-timestamp","elapsed_ms":12.5}')
            """))
            output = Path(directory) / "out.json"
            args = vscode_driver.argparse.Namespace(
                code=str(code), manifest=HERE / "vscode-baseline-manifest.json",
                output=output, samples=1, memory_delay=0.05, timeout=5, proc_root=Path("/proc"),
                workspace=None, fixture_file=None, window_probe=str(probe), open_probe=None, typing_probe=None,
                code_arg=[])
            status = vscode_driver.collect(args)
            envelope = json.loads(output.read_text())
        self.assertEqual(status, 0, envelope.get("error"))
        window = envelope["samples"][0]["scenarios"]["cold-start-window-visible"]
        self.assertEqual(window["source"], "compositor-timestamp")
        self.assertEqual(window["elapsed_ms"], 12.5)
        self.assertEqual(envelope["collection_status"], "partial")
        forged = json.loads(json.dumps(envelope))
        forged["samples"][0]["scenarios"]["typing-latency"] = {
            "status": "ok", "elapsed_ms": 4, "source": "vscode-self-report"}
        forged["summary"] = summarize(forged["samples"], MANIFEST)
        forged["collection_status"] = derive_status(forged["summary"])
        with self.assertRaises(vscode_validate.InvalidEvidence):
            validate_envelope(forged, MANIFEST)

    def test_rollup_parser_sums_the_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._write_pid(root, 7, rss=10, pss=4, children="8")
            self._write_pid(root, 8, rss=3, pss=3, children="")
            total = vscode_driver.process_tree_memory(root, 7)
        self.assertEqual(total["pss_bytes"], 7 * 1024)
        self.assertEqual(total["rss_bytes"], 13 * 1024)
        self.assertEqual(total["process_count"], 2)

    def _write_pid(self, root, pid, rss, pss, children):
        task = root / str(pid) / "task" / str(pid)
        task.mkdir(parents=True)
        (root / str(pid) / "smaps_rollup").write_text(f"Rss: {rss} kB\nPss: {pss} kB\n")
        (task / "children").write_text(children)


if __name__ == "__main__":
    unittest.main()
