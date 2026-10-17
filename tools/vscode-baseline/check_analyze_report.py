#!/usr/bin/env python3
"""Fail if a full Code-OSS port-analyze report is missing the M0 clearance anchors."""
import json
import sys
from pathlib import Path


REQUIRED_ELECTRON = ("app", "BrowserWindow", "protocol")
REQUIRED_NODE = ("fs", "child_process", "net")
REQUIRED_NATIVE = ("node-pty", "@vscode/sqlite3", "@parcel/watcher", "@vscode/ripgrep-universal", "vscode-oniguruma")


def main():
    report = json.loads(Path(sys.argv[1]).read_text())
    problems = []
    if report.get("schema") != "w3cos-port-analyze-v1":
        problems.append("unexpected schema")
    if report.get("files_scanned", 0) < 1000:
        problems.append(f"files_scanned {report.get('files_scanned')} is below 1000")
    apis = set(report.get("electron", {}).get("apis", []))
    for name in REQUIRED_ELECTRON:
        if name not in apis:
            problems.append(f"missing electron API {name}")
    builtins = set(report.get("node_builtins", {}))
    for name in REQUIRED_NODE:
        if name not in builtins:
            problems.append(f"missing node builtin {name}")
    natives = {item["name"] for item in report.get("native_modules", [])}
    for name in REQUIRED_NATIVE:
        if name not in natives:
            problems.append(f"missing native module {name}")
    if report.get("npmrc", {}).get("target") != "43.7.7":
        problems.append(f"npmrc target is {report.get('npmrc', {}).get('target')!r}")
    dynamic = sum(report.get("dynamic", {}).get("counts", {}).values())
    if dynamic < 1:
        problems.append("no product dynamic-code sites")
    clearance = set(report.get("clearance_list", []))
    if "electron:app" not in clearance or "native:node-pty" not in clearance or "node:fs" not in clearance:
        problems.append("clearance list is missing an anchor item")
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    summary = report["summary"]
    print(
        f"port analyze ok: files={report['files_scanned']} "
        f"electron={summary['electron_api_count']} node={summary['node_builtin_count']} "
        f"native={summary['native_module_count']} dynamic={summary['dynamic_product_count']} "
        f"clearance={summary['clearance_count']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
