#!/usr/bin/env python3
"""Chrome/control/candidate differential, isolating every native case."""
import argparse, hashlib, json, os, subprocess, time
from pathlib import Path
from playwright.sync_api import sync_playwright

p = argparse.ArgumentParser()
p.add_argument("--baseline", type=Path, required=True)
p.add_argument("--control", type=Path, required=True)
p.add_argument("--candidate", type=Path, required=True)
p.add_argument("--chrome", type=Path, required=True)
p.add_argument("--output", type=Path, required=True)
a = p.parse_args()
root = Path(__file__).resolve().parent
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
mark = "W3COS_PR55_REVIEW "

def parse(text):
    rows = []
    for line in text.splitlines():
        if line.startswith("[js.console.log] "):
            line = line[17:]
        if line.startswith(mark):
            rows.append(json.loads(line[len(mark):]))
    return rows

with sync_playwright() as pw:
    browser = pw.chromium.launch(executable_path=str(a.chrome), headless=True,
                                chromium_sandbox=True,
                                ignore_default_args=["--unsafely-disable-devtools-self-xss-warnings", "--enable-unsafe-swiftshader"])
    try:
        page = browser.new_context(offline=True, service_workers="block",
                                   viewport={"width": 1024, "height": 768},
                                   device_scale_factor=1).new_page()
        logs, errors = [], []
        page.on("console", lambda m: logs.append(m.text))
        page.on("pageerror", lambda m: errors.append(str(m)))
        page.set_content("<!doctype html><html><body></body></html>")
        page.add_script_tag(type="module", content=(root / "semantic-regressions.ts").read_text())
        deadline = time.monotonic() + 30
        while "W3COS_PR55_REVIEW_DONE" not in logs and time.monotonic() < deadline:
            page.wait_for_timeout(20)
        assert not errors and "W3COS_PR55_REVIEW_DONE" in logs, errors
        chrome = parse("\n".join(logs))
        version = browser.version
        (out / "chrome-console.log").write_text("\n".join(logs) + "\n")
    finally:
        browser.close()

records = {"chrome": chrome}
processes = {}
env = dict(os.environ, W3COS_JS_CONSOLE="1")
for key in ["W3COS_AOT_WINDOW", "W3COS_PERF_WORKLOAD"]:
    env.pop(key, None)
for label, binary in [("baseline", a.baseline), ("control", a.control), ("candidate", a.candidate)]:
    folder = out / label
    folder.mkdir()
    rows, cases = [], []
    for expected in chrome:
        name = expected["name"]
        r = subprocess.run([str(binary)], env=dict(env, W3COS_REVIEW_CASE=name),
                           capture_output=True, text=True, timeout=30)
        (folder / (name + ".stdout.log")).write_text(r.stdout)
        (folder / (name + ".stderr.log")).write_text(r.stderr)
        found = parse(r.stdout + "\n" + r.stderr)
        assert len(found) <= 1 and all(x["name"] == name for x in found)
        actual = found[0]["actual"] if found else {"process_failure": "stack_overflow" if "stack overflow" in r.stderr else "unobserved", "returncode": r.returncode}
        rows.append({"name": name, "actual": actual})
        cases.append({"name": name, "exit": r.returncode, "complete": "W3COS_PR55_REVIEW_DONE" in r.stdout + r.stderr})
    records[label] = rows
    processes[label] = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "cases": cases}
maps = {label: {r["name"]: r["actual"] for r in rows} for label, rows in records.items()}
result = []
old_failures = {"prototype-getter-before-after", "expando-delete", "matches-identity",
                "matches-borrowed-this", "matches-illegal-receiver", "fulfilled-await-order"}
for name, expected in maps["chrome"].items():
    row = {"name": name, **{label: mapping[name] for label, mapping in maps.items()}}
    row["status"] = "PASS" if row["candidate"] == expected else ("OLD_BASELINE_FAILURE" if name in old_failures and row["candidate"] == row["baseline"] else "REGRESSION_OR_UNRESOLVED")
    row["pr55_new_regression_fixed"] = row["baseline"] == expected and row["control"] != expected and row["candidate"] == expected
    result.append(row)
failures = [r for r in result if r["status"] == "REGRESSION_OR_UNRESOLVED"]
report = {"source_sha256": hashlib.sha256((root / "semantic-regressions.ts").read_bytes()).hexdigest(),
          "chrome_version": version, "sandbox": True, "rows": result, "processes": processes,
          "pass": len([r for r in result if r["status"] == "PASS"]),
          "old_baseline_failures": [r["name"] for r in result if r["status"] == "OLD_BASELINE_FAILURE"],
          "unresolved": [r["name"] for r in failures], "correctness_gate": not failures}
(out / "comparison.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps({k: report[k] for k in ["pass", "old_baseline_failures", "unresolved", "correctness_gate"]}))
raise SystemExit(bool(failures))
