#!/usr/bin/env python3
"""Collect fresh-process DOM API evidence. Does not install dependencies or relax sandboxes."""
import argparse
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import subprocess
import time
import uuid

from validate import compare, digest, file_digest, load_manifest, load_json, require, validate_result, validate_native_configuration

MARKER = "W3COS_PERF_RESULT "


def parse_result(lines):
    matches = []
    for line in lines:
        # Native console is stderr-prefixed; browser console exposes plain text.
        if line.startswith("[js.console.log] "):
            line = line[len("[js.console.log] "):]
        if line.startswith(MARKER):
            matches.append(line[len(MARKER):])
    require(len(matches) == 1, "Expected exactly one complete fixture result record")
    return load_json(matches[0])


def browser_run(chromium, fixture, manifest):
    from playwright.sync_api import sync_playwright
    lines = []
    with sync_playwright() as playwright:
        # Keep Chromium's sandbox enabled; never add --no-sandbox to work around
        # an execution-environment denial. Fail closed if launch is unavailable.
        browser = playwright.chromium.launch(
            executable_path=str(chromium), headless=True, chromium_sandbox=True,
            ignore_default_args=["--unsafely-disable-devtools-self-xss-warnings", "--enable-unsafe-swiftshader"])
        try:
            context = browser.new_context(
                viewport={"width": manifest["viewport"]["width"], "height": manifest["viewport"]["height"]},
                device_scale_factor=manifest["viewport"]["dpr"], offline=True, service_workers="block")
            page = context.new_page()
            page.on("console", lambda message: lines.append(message.text))
            page.set_content("<!doctype html><html><head><meta charset='utf-8'></head><body></body></html>")
            # type=module executes the exact .ts bytes (they deliberately use
            # only JavaScript syntax), matching the native ESM/AOT source.
            page.add_script_tag(type="module", content=fixture.read_text())
            deadline = time.monotonic() + 60
            while not any(line.startswith(MARKER) for line in lines) and time.monotonic() < deadline:
                page.wait_for_timeout(20)
            result = parse_result(lines)
            version = browser.version + "; playwright=" + importlib.metadata.version("playwright")
            return result, version, {"sandbox_enabled": True, "offline": True, "headless": True, "service_workers": "block",
                                     "ignored_default_args": ["--unsafely-disable-devtools-self-xss-warnings", "--enable-unsafe-swiftshader"],
                                     "playwright_version": importlib.metadata.version("playwright"),
                                     "chromium_version": browser.version, "viewport": manifest["viewport"],
                                     "executable_sha256": file_digest(chromium)}
        finally:
            browser.close()


def verify_native(binary, provenance_path, fixture_hash):
    provenance = load_json(provenance_path.read_text())
    validate_native_configuration(provenance, fixture_hash)
    require(provenance["binary_sha256"] == file_digest(binary), "Native binary differs from build provenance")
    return provenance


def native_run(binary, provenance_path, fixture_hash):
    provenance = verify_native(binary, provenance_path, fixture_hash)
    environment = dict(os.environ)
    environment.pop("W3COS_AOT_WINDOW", None)
    environment["W3COS_JS_CONSOLE"] = "1"
    process = subprocess.run([str(binary.resolve())], env=environment, capture_output=True, text=True, timeout=60)
    require(process.returncode == 0, "Native process failed: " + process.stderr[-1000:])
    result = parse_result(process.stderr.splitlines())
    return result, provenance["runtime_git_revision"] + "; rustc=" + provenance["compiler_version"], provenance


def collect(args):
    manifest = load_manifest(args.manifest)
    fixture = args.manifest.parent / manifest["fixture"]
    fixture_hash = file_digest(fixture)
    session = str(uuid.uuid4())
    envelope = {"schema": "w3cos-paired-evidence-v1", "collection_status": "incomplete",
                "fixture_sha256": fixture_hash, "manifest_sha256": digest(manifest),
                "host_session": session, "host": {"platform": platform.platform(), "machine": platform.machine()},
                "runs": []}
    try:
        if args.mode == "paired":
            require(args.native_binary is not None and args.native_provenance is not None, "Paired mode requires native binary and build provenance")
            envelope["native_build"] = verify_native(args.native_binary, args.native_provenance, fixture_hash)
        for pair in range(manifest["pairs"] if args.mode == "paired" else 1):
            engines = (["native", "browser"] if pair % 2 == 0 else ["browser", "native"]) if args.mode == "paired" else ["browser"]
            for engine in engines:
                require(file_digest(fixture) == fixture_hash, "Fixture changed during collection")
                result, version, configuration = (native_run(args.native_binary, args.native_provenance, fixture_hash)
                                   if engine == "native" else browser_run(args.chromium, fixture, manifest))
                validate_result(result, manifest)
                envelope["runs"].append({"run_id": str(uuid.uuid4()), "pair": pair,
                    "order": len(envelope["runs"]), "engine": engine, "host_session": session,
                    "fixture_sha256": fixture_hash, "engine_version": version,
                    "execution_mode": "esm-aot-headless-release" if engine == "native" else "chromium-headless",
                    "engine_configuration": configuration, "configuration_sha256": digest(configuration),
                    "result_sha256": digest(result), "result": result})
                # Persist partial evidence; interruption must never become a pass.
                args.output.write_text(json.dumps(envelope, indent=2, allow_nan=False) + "\n")
        envelope["collection_status"] = "complete" if args.mode == "paired" else "browser-only"
    except Exception as error:
        envelope["collection_status"] = "blocked"
        envelope["error"] = type(error).__name__ + ": " + str(error)
    args.output.write_text(json.dumps(envelope, indent=2, allow_nan=False) + "\n")
    report = compare(envelope, manifest, fixture_hash)
    print(json.dumps(report, indent=2, allow_nan=False))
    return 0 if report["status"] == "pass" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["browser", "paired"])
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("manifest.json"))
    parser.add_argument("--chromium", type=Path, default=Path("/usr/bin/chromium"))
    parser.add_argument("--native-binary", type=Path)
    parser.add_argument("--native-provenance", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        return collect(args)
    except (OSError, ValueError, TypeError) as error:
        print(json.dumps({"status": "blocked", "reason": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
