#!/usr/bin/env python3
"""Collect the stock Electron Code-OSS A-side baseline.

Workbench-ready time comes from VS Code's own `--prof-duration-markers`
(`code/didStartMain` to `code/didStartWorkbench`). Memory is the sum of
Pss/Rss in `/proc/<pid>/smaps_rollup` across the process tree. Window
visibility, open-file first text, and typing latency are recorded only when
an external probe prints them. Missing probes stay blocked. This driver does
not invent timings and does not compare against w3cos.
"""
import argparse
import hashlib
import json
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from vscode_validate import (
    MANIFEST,
    derive_status,
    digest,
    load_json,
    load_manifest,
    summarize,
    validate_envelope,
)


MARKER = MANIFEST["workbench_marker"]


def file_digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def parse_rollup(text):
    fields = {}
    for line in text.splitlines():
        if line.startswith("Rss:") or line.startswith("Pss:"):
            key, value, unit = line.split()[:3]
            if unit != "kB" or not value.isdigit():
                continue
            fields[key[:-1]] = int(value)
    return fields


def child_pids(proc_root, pid):
    path = Path(proc_root) / str(pid) / "task" / str(pid) / "children"
    if not path.is_file():
        return []
    children = []
    for item in path.read_text().split():
        if item.isdigit():
            children.append(int(item))
    return children


def process_tree_memory(proc_root, pid):
    stack = [pid]
    seen = set()
    pss = rss = count = 0
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        rollup = Path(proc_root) / str(current) / "smaps_rollup"
        if rollup.is_file():
            fields = parse_rollup(rollup.read_text())
            if "Pss" in fields and "Rss" in fields:
                pss += fields["Pss"] * 1024
                rss += fields["Rss"] * 1024
                count += 1
        stack.extend(child_pids(proc_root, current))
    if count == 0:
        return None
    return {"pss_bytes": pss, "rss_bytes": rss, "process_count": count, "source": "proc-smaps-rollup", "status": "ok"}


def parse_duration_markers(text):
    pairs = {}
    for line in text.splitlines():
        parts = line.split("\t")
        if not parts or len(parts) % 2:
            continue
        for index in range(0, len(parts), 2):
            name, raw = parts[index], parts[index + 1]
            if name and raw.isdigit():
                pairs[name] = int(raw)
    return pairs


def parse_append_timers(text):
    lines = [line for line in text.splitlines() if line.strip()]
    if not lines:
        return {}
    columns = lines[-1].split("\t")
    parsed = {}
    if columns and columns[0].isdigit():
        parsed["upstream_ellapsed_ms"] = int(columns[0])
    if len(columns) >= 5:
        parsed["standard_start"] = columns[4] == "standard_start"
        parsed["standard_detail"] = columns[4]
    if len(columns) >= 3:
        parsed["commit_prefix"] = columns[2]
    return parsed


def blocked(reason):
    return {"status": "blocked", "reason": reason}


def error(reason):
    return {"status": "error", "reason": reason}


def parse_probe_output(stdout, stderr, returncode):
    if returncode != 0:
        detail = (stderr or stdout or "probe failed").strip()
        return error(detail[-300:])
    payload = None
    for line in stdout.splitlines():
        if line.startswith("{"):
            try:
                payload = load_json(line)
            except (json.JSONDecodeError, ValueError):
                return error("probe stdout is not the expected JSON line")
    if not isinstance(payload, dict):
        return error("probe did not print a JSON object")
    if payload.get("schema") != "w3cos-vscode-probe-v1":
        return error("probe schema is not w3cos-vscode-probe-v1")
    if "elapsed_ms" not in payload or "source" not in payload:
        return error("probe JSON is missing elapsed_ms or source")
    return {"status": "ok", "elapsed_ms": payload["elapsed_ms"], "source": payload["source"]}


def finish_probe(process, timeout):
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate()
        return error("probe timed out")
    except OSError as exc:
        return error(str(exc))
    return parse_probe_output(stdout, stderr, process.returncode)


def run_sample(code, command, probes, timeout, memory_delay, proc_root):
    """Start Code, then the probes, and sample memory while that process is alive."""
    memory = None
    launch_error = None
    process = subprocess.Popen(
        [str(code), *command], start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    probe_env_extra = {"W3COS_VSCODE_PID": str(process.pid)}
    started = []
    for command_text, env in probes:
        probe_env = dict(env)
        probe_env.update(probe_env_extra)
        try:
            started.append(subprocess.Popen(
                shlex.split(command_text), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=probe_env))
        except OSError as exc:
            started.append(exc)
    try:
        time.sleep(memory_delay)
        memory = process_tree_memory(proc_root, process.pid)
        deadline = time.monotonic() + timeout
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.05)
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
            launch_error = "code process exceeded the sample timeout"
    except OSError as exc:
        launch_error = str(exc)
    remaining = max(1.0, timeout)
    results = []
    for probe in started:
        if isinstance(probe, OSError):
            results.append(error(str(probe)))
        else:
            results.append(finish_probe(probe, remaining))
    return memory, launch_error, results


def one_sample(index, args, manifest):
    sample_dir = Path(tempfile.mkdtemp(prefix="w3cos-vscode-baseline-"))
    user_data = sample_dir / "user-data"
    user_data.mkdir()
    duration_path = sample_dir / "duration-markers.txt"
    append_path = sample_dir / "append-timers.txt"
    command = [
        *manifest["launch_args"],
        "--user-data-dir", str(user_data),
        "--prof-duration-markers", "ellapsed",
        "--prof-duration-markers", MARKER,
        "--prof-duration-markers-file", str(duration_path),
        "--prof-append-timers", str(append_path),
    ]
    if args.workspace:
        command.append(str(args.workspace))
    command.extend(args.code_arg)
    probe_env = os.environ.copy()
    probe_env["W3COS_VSCODE_CODE"] = str(Path(args.code).resolve())
    probe_env["W3COS_VSCODE_USER_DATA_DIR"] = str(user_data)
    if args.fixture_file:
        probe_env["W3COS_VSCODE_FIXTURE_FILE"] = str(args.fixture_file)
    probe_specs = [
        ("cold-start-window-visible", args.window_probe, "window-probe-not-configured"),
        ("open-file", args.open_probe, "open-probe-not-configured"),
        ("typing-latency", args.typing_probe, "typing-probe-not-configured"),
    ]
    active = [(command_text, probe_env) for _scenario, command_text, _reason in probe_specs if command_text]
    memory, launch_error, probe_results = run_sample(
        args.code, command, active, args.timeout, args.memory_delay, args.proc_root)
    scenarios = {}
    if launch_error:
        scenarios["cold-start-workbench-ready"] = error(launch_error)
    elif not duration_path.is_file():
        scenarios["cold-start-workbench-ready"] = error("prof-duration-markers file was not written")
    else:
        pairs = parse_duration_markers(duration_path.read_text())
        elapsed = pairs.get(MARKER)
        if elapsed is None:
            scenarios["cold-start-workbench-ready"] = error(f"missing marker {MARKER}")
        else:
            row = {
                "status": "ok",
                "elapsed_ms": elapsed,
                "marker": MARKER,
                "source": "vscode-prof-duration-markers",
            }
            if append_path.is_file():
                row.update(parse_append_timers(append_path.read_text()))
            scenarios["cold-start-workbench-ready"] = row
    if memory is None:
        scenarios["memory-pss"] = error("smaps_rollup was not readable for the process tree")
    else:
        scenarios["memory-pss"] = memory
    result_cursor = 0
    for scenario_id, command_text, reason in probe_specs:
        if not command_text:
            scenarios[scenario_id] = blocked(reason)
        else:
            scenarios[scenario_id] = probe_results[result_cursor]
            result_cursor += 1
    ordered = {scenario_id: scenarios[scenario_id] for scenario_id in (item["id"] for item in manifest["scenarios"])}
    return {"index": index, "scenarios": ordered, "argv": command}


def collect(args):
    manifest = load_manifest(args.manifest)
    code_path = Path(args.code) if args.code else None
    envelope = {
        "schema": "w3cos-vscode-baseline-evidence-v1",
        "side": "A",
        "reference": manifest["reference"],
        "collection_status": "blocked",
        "manifest_sha256": digest(manifest),
        "code": None,
        "host": {"platform": os.uname().sysname, "machine": os.uname().machine},
        "samples": [],
        "summary": {},
    }
    if code_path is None or not code_path.is_file() or not os.access(code_path, os.X_OK):
        envelope["error"] = "Code-OSS executable is missing or not executable"
        envelope["summary"] = {}
        _write(args.output, envelope)
        print(json.dumps({"status": "blocked", "error": envelope["error"]}, indent=2))
        return 1
    envelope["code"] = {"path": str(code_path.resolve()), "sha256": file_digest(code_path)}
    if args.samples < 1:
        envelope["error"] = "--samples must be >= 1"
        _write(args.output, envelope)
        return 1
    samples = [one_sample(index, args, manifest) for index in range(args.samples)]
    summary = summarize(samples, manifest)
    envelope["samples"] = samples
    envelope["summary"] = summary
    envelope["collection_status"] = derive_status(summary)
    try:
        validate_envelope(envelope, manifest)
    except ValueError as exc:
        envelope["collection_status"] = "blocked"
        envelope["error"] = "evidence rejected: " + str(exc)
        _write(args.output, envelope)
        print(json.dumps({"status": "blocked", "error": envelope["error"]}, indent=2))
        return 1
    _write(args.output, envelope)
    print(json.dumps({"status": envelope["collection_status"], "summary": summary}, indent=2))
    return 0 if envelope["collection_status"] != "blocked" else 1


def _write(path, envelope):
    Path(path).write_text(json.dumps(envelope, indent=2, allow_nan=False) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--code", help="Stock Code-OSS executable (side A)")
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("vscode-baseline-manifest.json"))
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--samples", type=int, default=1)
    parser.add_argument("--memory-delay", type=float, default=0.3)
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--proc-root", type=Path, default=Path("/proc"))
    parser.add_argument("--workspace", type=Path)
    parser.add_argument("--fixture-file", type=Path)
    parser.add_argument("--window-probe")
    parser.add_argument("--open-probe")
    parser.add_argument("--typing-probe")
    parser.add_argument("--code-arg", action="append", default=[])
    args = parser.parse_args()
    return collect(args)


if __name__ == "__main__":
    raise SystemExit(main())
