#!/usr/bin/env python3
"""Fail-closed checks for the stock Code-OSS A-side baseline.

A complete envelope needs 20 samples of every scenario from the evidence
source named in the manifest. Application-reported numbers are accepted only
for the workbench-ready marker. Window, open-file, and typing stay blocked
until an external probe supplies them. This module never invents a duration.
"""
import hashlib
import json
import math
from pathlib import Path


class InvalidEvidence(ValueError):
    pass


MANIFEST = {
    "schema": "w3cos-vscode-baseline-manifest-v1",
    "side": "A",
    "reference": "stock-electron-code-oss",
    "samples_for_complete": 20,
    "percentiles": ["median", "p95"],
    "launch_args": ["--disable-extensions", "--skip-release-notes", "--disable-workspace-trust"],
    "workbench_marker": "code/didStartMain-code/didStartWorkbench",
    "scenarios": [
        {
            "id": "cold-start-window-visible",
            "metric": "exec-to-first-frame-ms",
            "evidence": "compositor-or-pixel-probe",
            "allows_application_report": False,
        },
        {
            "id": "cold-start-workbench-ready",
            "metric": "exec-to-did-start-workbench-ms",
            "evidence": "vscode-prof-duration-markers",
            "allows_application_report": True,
        },
        {
            "id": "open-file",
            "metric": "quick-open-to-first-text-frame-ms",
            "evidence": "compositor-or-pixel-probe",
            "allows_application_report": False,
        },
        {
            "id": "typing-latency",
            "metric": "key-to-pixel-ms",
            "evidence": "evdev-pixel-probe",
            "allows_application_report": False,
        },
        {
            "id": "memory-pss",
            "metric": "process-tree-pss-bytes",
            "evidence": "proc-smaps-rollup",
            "allows_application_report": False,
        },
    ],
}

ALLOWED_SOURCES = {
    "compositor-or-pixel-probe": {"compositor-timestamp", "pixel-probe"},
    "evdev-pixel-probe": {"evdev-pixel-probe"},
    "vscode-prof-duration-markers": {"vscode-prof-duration-markers"},
    "proc-smaps-rollup": {"proc-smaps-rollup"},
}
SCENARIO_IDS = [item["id"] for item in MANIFEST["scenarios"]]
TIME_SCENARIOS = {
    "cold-start-window-visible",
    "cold-start-workbench-ready",
    "open-file",
    "typing-latency",
}


def require(condition, message):
    if not condition:
        raise InvalidEvidence(message)


def load_json(text):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "Duplicate JSON object key: " + key)
            result[key] = value
        return result

    def reject_constant(value):
        raise InvalidEvidence("Nonstandard JSON numeric constant: " + value)

    return json.loads(text, object_pairs_hook=unique_object, parse_constant=reject_constant)


def digest(value):
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)
    return hashlib.sha256(payload.encode()).hexdigest()


def load_manifest(path):
    manifest = load_json(Path(path).read_text())
    require(manifest == MANIFEST, "Unknown, incomplete, or modified VS Code baseline manifest")
    require(digest(manifest) == digest(MANIFEST), "Manifest field types differ")
    return manifest


def finite_number(value):
    return type(value) in (int, float) and not isinstance(value, bool) and math.isfinite(value)


def percentile(values, pct):
    ordered = sorted(values)
    rank = math.ceil(pct / 100 * len(ordered)) - 1
    return ordered[max(0, min(len(ordered) - 1, rank))]


def scenario_by_id(manifest):
    return {item["id"]: item for item in manifest["scenarios"]}


def summarize(samples, manifest):
    needed = manifest["samples_for_complete"]
    specs = scenario_by_id(manifest)
    summary = {}
    for scenario_id in SCENARIO_IDS:
        rows = [sample["scenarios"][scenario_id] for sample in samples]
        ok = [row for row in rows if row["status"] == "ok"]
        errors = [row for row in rows if row["status"] == "error"]
        if len(ok) >= needed:
            status = "ok"
        elif ok:
            status = "insufficient"
        elif errors:
            status = "error"
        else:
            status = "blocked"
        item = {"status": status, "ok_samples": len(ok), "source": specs[scenario_id]["evidence"]}
        if ok and scenario_id in TIME_SCENARIOS:
            values = [row["elapsed_ms"] for row in ok]
            item["median_ms"] = percentile(values, 50)
            item["p95_ms"] = percentile(values, 95)
        elif ok and scenario_id == "memory-pss":
            values = [row["pss_bytes"] for row in ok]
            item["median_bytes"] = percentile(values, 50)
            item["p95_bytes"] = percentile(values, 95)
        elif status == "blocked":
            reasons = sorted({row.get("reason", "") for row in rows})
            item["reason"] = reasons[0] if len(reasons) == 1 else "mixed-blocked"
        summary[scenario_id] = item
    return summary


def derive_status(summary):
    statuses = [item["status"] for item in summary.values()]
    if statuses and all(status == "ok" for status in statuses):
        return "complete"
    if any(status in ("ok", "insufficient") for status in statuses):
        return "partial"
    return "blocked"


def _check_time_row(row, spec):
    require(finite_number(row["elapsed_ms"]) and row["elapsed_ms"] > 0, "elapsed_ms must be a positive finite number")
    require(row["elapsed_ms"] < 600_000, "elapsed_ms is above the 10 minute sanity cap")
    require(row["source"] in ALLOWED_SOURCES[spec["evidence"]], "time sample source is not allowed for this scenario")
    if not spec["allows_application_report"]:
        require(row["source"] != "vscode-prof-duration-markers", "application startup markers cannot satisfy this scenario")
        require("vscode-self-report" not in row["source"], "application self-report cannot satisfy this scenario")
    if spec["id"] == "cold-start-workbench-ready":
        require(row["marker"] == MANIFEST["workbench_marker"], "workbench sample used a different marker")


def _check_memory_row(row, spec):
    require(type(row["pss_bytes"]) is int and row["pss_bytes"] > 0, "pss_bytes must be a positive int")
    require(type(row["rss_bytes"]) is int and row["rss_bytes"] >= 0, "rss_bytes must be a non-negative int")
    require(type(row["process_count"]) is int and row["process_count"] >= 1, "process_count must be a positive int")
    require(row["source"] in ALLOWED_SOURCES[spec["evidence"]], "memory sample source is not proc-smaps-rollup")


def validate_envelope(envelope, manifest):
    require(envelope.get("schema") == "w3cos-vscode-baseline-evidence-v1", "unexpected evidence schema")
    require(envelope.get("side") == "A", "evidence is not side A")
    require(envelope.get("reference") == manifest["reference"], "evidence reference changed")
    require(envelope.get("manifest_sha256") == digest(manifest), "manifest hash mismatch")
    samples = envelope.get("samples")
    require(isinstance(samples, list), "samples must be a list")
    seen = set()
    for index, sample in enumerate(samples):
        require(sample.get("index") == index, "sample index is missing or out of order")
        require(sample["index"] not in seen, "duplicate sample index")
        seen.add(sample["index"])
        scenarios = sample.get("scenarios")
        require(isinstance(scenarios, dict) and list(scenarios) == SCENARIO_IDS, "sample scenarios do not match the manifest")
        for spec in manifest["scenarios"]:
            row = scenarios[spec["id"]]
            require(row.get("status") in ("ok", "blocked", "error"), "unknown sample status")
            if row["status"] == "blocked":
                require(isinstance(row.get("reason"), str) and row["reason"], "blocked sample needs a reason")
                require("elapsed_ms" not in row and "pss_bytes" not in row, "blocked sample contains a measurement")
            elif row["status"] == "error":
                require(isinstance(row.get("reason"), str) and row["reason"], "error sample needs a reason")
            elif spec["id"] == "memory-pss":
                _check_memory_row(row, spec)
            else:
                _check_time_row(row, spec)
    summary = summarize(samples, manifest)
    require(envelope.get("summary") == summary, "summary does not match the samples")
    require(envelope.get("collection_status") == derive_status(summary), "collection_status does not match the samples")
    if any(item["status"] == "ok" for item in summary.values()):
        code = envelope.get("code")
        require(isinstance(code, dict) and len(code.get("sha256", "")) == 64, "measured evidence needs the code binary sha256")
    return envelope["collection_status"]
