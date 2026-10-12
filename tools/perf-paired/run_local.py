#!/usr/bin/env python3
"""Preflight and collect the pinned same-host native/Chromium DOM API benchmark.

Requires an existing release w3cos CLI and installed Python Playwright, Chromium,
Git, Cargo and native build tools. Never installs prerequisites or browsers and
never disables the Chromium sandbox. Normal builds may fetch Cargo dependencies;
--offline forbids fetching and requires those exact dependencies to be cached.
Only the two declared synchronous DOM script wall-time scenarios are measured.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import uuid

# Importing helpers must not create untracked source files mid-build.
sys.dont_write_bytecode = True

from validate import digest, file_digest, load_json, load_manifest, require

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
SCOPE = "Only append-remove-pool and class-matches synchronous DOM script wall time; not frame/display/app performance"


def executable(path):
    return path.is_file() and os.access(path, os.X_OK)


def find_chromium(explicit=None):
    """Discover installed browsers only. An explicit invalid path never falls back."""
    if explicit is not None:
        path = explicit.expanduser().absolute()
        require(executable(path), "Chrome/Chromium executable is missing or not executable: " + str(path))
        return path
    app_roots = [Path("/Applications"), Path.home() / "Applications"]
    candidates = [root / app / "Contents/MacOS" / binary
                  for app, binary in [("Google Chrome.app", "Google Chrome"), ("Chromium.app", "Chromium")]
                  for root in app_roots]
    candidates += [Path(found) for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"]
                   if (found := shutil.which(name))]
    for path in candidates:
        if executable(path):
            return path.absolute()
    raise ValueError("No installed Chrome/Chromium found. Supply --chromium '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' or another installed executable.")


def probe(command, repo, environment):
    result = subprocess.run(command, cwd=repo, env=environment, capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, "Command failed: " + " ".join(command) + ": " + (result.stderr or result.stdout).strip())
    return result.stdout.strip()


def inspect_lock(path):
    import tomllib
    require(path.is_file(), "Missing pinned native lockfile: " + str(path))
    lock = tomllib.loads(path.read_text())
    require(type(lock.get("version")) is int and lock["version"] in (3, 4)
            and isinstance(lock.get("package"), list) and lock["package"], "Invalid pinned native lockfile")
    return file_digest(path)


def preflight(args):
    """Read/probe prerequisites. Does not create output, compile, fetch or launch a browser."""
    repo = REPO
    environment = dict(os.environ)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    if args.offline:
        environment["CARGO_NET_OFFLINE"] = "true"
    output = args.output_directory.expanduser().resolve()
    cli = args.w3cos.expanduser().absolute()
    lockfile = args.lockfile.expanduser().resolve()
    report = {"status": "blocked", "scope": SCOPE, "checks": [],
              "output_directory": str(output), "offline": args.offline,
              "limitations": ["No browser is launched and no native code is compiled by preflight.",
                              "Native system libraries, the complete dependency cache and sandbox availability remain unverified.",
                              "Default builds/fetch may download Cargo dependencies; --offline requires cached exact versions."]}
    context = {"repo": repo, "output": output, "cli": cli, "lockfile": lockfile,
               "environment": environment, "manifest_path": HERE / "manifest.json"}

    def check(name, callback):
        try:
            detail = callback()
            report["checks"].append({"name": name, "status": "ok", "detail": detail})
            return detail
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            report["checks"].append({"name": name, "status": "blocked", "detail": str(error)})
            return None

    def python_version():
        require(sys.version_info >= (3, 11), "Python 3.11+ is required; run this script with python3.11 or newer.")
        return sys.version.split()[0]

    # Keep the version failure readable on macOS's older system Python, before
    # importing build helpers that depend on Python 3.11's tomllib.
    if check("python", python_version) is None:
        return report, context

    def fixture():
        manifest = load_manifest(context["manifest_path"])
        path = HERE / manifest["fixture"]
        require(path.is_file(), "Missing pinned fixture: " + str(path))
        context["manifest_hash"] = digest(manifest)
        context["fixture_hash"] = file_digest(path)
        return {"manifest_sha256": context["manifest_hash"], "fixture_sha256": context["fixture_hash"],
                "pairs": manifest["pairs"], "warmup": manifest["parameters"]["warmup"],
                "samples": manifest["parameters"]["samples"]}

    check("pinned_fixture", fixture)
    context["lock_hash"] = check("pinned_native_lock", lambda: inspect_lock(lockfile))
    check("git", lambda: probe(["git", "-C", str(repo), "rev-parse", "--show-toplevel"], repo, environment))

    def destination():
        require(not output.exists(), "Output directory already exists; choose a fresh directory to preserve earlier evidence: " + str(output))
        require(not output.is_relative_to(repo),
                "Output must be outside the repository so generated projects do not inherit its Cargo workspace.")
        import tomllib
        for parent in output.parents:
            manifest = parent / "Cargo.toml"
            if manifest.is_file():
                require("workspace" not in tomllib.loads(manifest.read_text()),
                        "Output must be outside every Cargo workspace: " + str(manifest))
        parent = next(parent for parent in output.parents if parent.exists())
        require(parent.is_dir() and os.access(parent, os.W_OK | os.X_OK), "Output parent is not writable: " + str(parent))
        return str(output)

    check("output_directory", destination)

    def build_environment():
        from rebuild_native_locked import check_build_environment
        checked = dict(environment)
        checked.pop("CARGO_TARGET_DIR", None)
        checked.pop("CARGO_BUILD_TARGET", None)
        checked["CARGO_NET_OFFLINE"] = "true"
        checked["CARGO_TARGET_DIR"] = str(output / "native/compiler-temp-preflight/w3cos-build/target")
        check_build_environment(output / "native/compiler-temp-preflight/w3cos-build", checked)
        return "No unsupported Cargo configuration, toolchain file, compiler/profile/target override"

    check("locked_build_environment", build_environment)
    check("cargo", lambda: probe(["cargo", "--version"], repo, environment))
    check("rustc", lambda: probe(["rustc", "--version"], repo, environment))

    def cli_version():
        require(executable(cli), "Release w3cos CLI is missing or not executable: " + str(cli)
                + ". Build it with the repository's documented release build, or pass --w3cos /path/to/w3cos.")
        version = probe([str(cli), "--version"], repo, environment)
        help_text = probe([str(cli), "build", "--help"], repo, environment)
        require("--release" in help_text and "--output" in help_text, "w3cos build does not support the required --release/--output options")
        return {"path": str(cli), "version": version}

    check("w3cos_cli", cli_version)
    check("playwright", lambda: probe([sys.executable, "-c",
          "from playwright.sync_api import sync_playwright; import importlib.metadata; print(importlib.metadata.version('playwright'))"], repo, environment))

    def browser():
        path = find_chromium(args.chromium)
        context["chromium"] = path
        return {"path": str(path), "launch_checked": False}

    check("chromium", browser)
    if platform.system() == "Darwin":
        check("macos_command_line_tools", lambda: probe(["xcode-select", "--print-path"], repo, environment))
    report["status"] = "ready" if all(item["status"] == "ok" for item in report["checks"]) else "blocked"
    return report, context


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def run_stage(name, command, context, report, timeout):
    output = context["output"]
    record = {"name": name, "command": command, "working_directory": str(context["repo"]),
              "stdout": str(output / (name + ".stdout.log")), "stderr": str(output / (name + ".stderr.log")),
              "status": "incomplete"}
    report["stages"].append(record)
    write_json(output / "local-run.json", report)
    print("Running " + name + "; logs: " + record["stdout"], file=sys.stderr, flush=True)
    with Path(record["stdout"]).open("w") as stdout, Path(record["stderr"]).open("w") as stderr:
        try:
            result = subprocess.run(command, cwd=context["repo"], env=context["environment"],
                                    stdout=stdout, stderr=stderr, text=True, timeout=timeout)
            record["returncode"] = result.returncode
            record["status"] = "finished"
        except (OSError, subprocess.SubprocessError) as error:
            record["status"] = "blocked"
            record["error"] = str(error)
            raise
        finally:
            write_json(output / "local-run.json", report)
    return result.returncode


def prepare_dependency_fetch(context, native):
    """Stage only a verified manifest plus the explicit pin; leave originals intact."""
    from rebuild_native_locked import generated_inputs, regular_path
    from run import verify_native
    provenance = verify_native(native / "w3cos-dom-api-bench", native / "build-provenance.json", context["fixture_hash"])
    projects = [path for path in native.glob("compiler-temp-*/w3cos-build")
                if all((path / name).is_file() and file_digest(path / name) == provenance[field]
                       for name, field in [("src/main.rs", "generated_main_sha256"),
                                           ("src/esm_bundle.rs", "generated_esm_sha256"),
                                           ("Cargo.lock", "lockfile_sha256")])]
    require(len(projects) == 1, "Need exactly one generated project matching native provenance before dependency fetch")
    project = regular_path(projects[0], directory=True)
    generated_inputs(project, context["repo"])
    require(file_digest(context["lockfile"]) == context["lock_hash"], "Pinned lockfile changed after preflight")
    staging = context["output"] / "pinned-dependencies"
    (staging / "src").mkdir(parents=True)
    shutil.copy2(project / "Cargo.toml", staging / "Cargo.toml")
    shutil.copy2(context["lockfile"], staging / "Cargo.lock")
    # Cargo fetch needs a target declaration; this dummy target is never built.
    (staging / "src/main.rs").write_text("fn main() {}\n")
    return staging


def check_pins(context):
    require(digest(load_manifest(context["manifest_path"])) == context["manifest_hash"], "Manifest changed after preflight")
    require(file_digest(HERE / "scenarios.ts") == context["fixture_hash"], "Fixture changed after preflight")
    require(file_digest(context["lockfile"]) == context["lock_hash"], "Pinned lockfile changed after preflight")


def execute(preflight_report, context, offline):
    output = context["output"]
    # Exclusive leaf creation: never overwrite evidence, even after two
    # simultaneous invocations both passed preflight.
    output.mkdir(parents=True, exist_ok=False)
    write_json(output / "preflight.json", preflight_report)
    report = {"schema": "w3cos-local-paired-run-v1", "status": "incomplete", "scope": SCOPE,
              "output_directory": str(output), "offline": offline, "stages": []}
    write_json(output / "local-run.json", report)
    native = output / "native"
    evidence = output / "paired-evidence.json"
    try:
        check_pins(context)
        command = [sys.executable, str(HERE / "build_native.py"), "--w3cos", str(context["cli"]),
                   "--output-directory", str(native), "--repo-root", str(context["repo"]),
                   "--manifest", str(context["manifest_path"])]
        require(run_stage("build", command, context, report, 1900) == 0,
                "Native build failed; inspect build.stderr.log and native/build.log. No benchmark was collected.")
        check_pins(context)
        if not offline:
            staging = prepare_dependency_fetch(context, native)
            command = ["cargo", "fetch", "--locked", "--manifest-path", str(staging / "Cargo.toml")]
            require(run_stage("fetch-pinned-dependencies", command, context, report, 1800) == 0,
                    "Pinned dependency fetch failed; inspect fetch-pinned-dependencies.stderr.log. No lockfile was relaxed.")
            require(file_digest(staging / "Cargo.lock") == context["lock_hash"], "Cargo fetch changed the pinned staging lock")
        check_pins(context)
        command = [sys.executable, str(HERE / "rebuild_native_locked.py"), "--build-directory", str(native),
                   "--repo-root", str(context["repo"]), "--lockfile", str(context["lockfile"])]
        require(run_stage("locked-rebuild", command, context, report, 1900) == 0,
                "Pinned native rebuild failed; inspect locked-rebuild.stdout.log, locked-rebuild.stderr.log and native/locked-rebuild-history. "
                "Offline runs require the exact pinned dependency cache. Preserve this output and retry with a fresh output directory.")
        check_pins(context)
        from run import verify_native
        rebuilt = verify_native(native / "w3cos-dom-api-bench", native / "build-provenance.json", context["fixture_hash"])
        require(rebuilt["lockfile_sha256"] == context["lock_hash"], "Rebuilt native provenance differs from the selected pinned lock")
        command = [sys.executable, str(HERE / "run.py"), "paired", "--chromium", str(context["chromium"]),
                   "--native-binary", str(native / "w3cos-dom-api-bench"),
                   "--native-provenance", str(native / "build-provenance.json"),
                   "--manifest", str(context["manifest_path"]), "--output", str(evidence)]
        collected = run_stage("collect", command, context, report, 2400)
        # A nonzero collector exit can be a valid fail/inconclusive comparison.
        # Always run the independent validator over retained partial/full evidence.
        command = [sys.executable, str(HERE / "validate.py"), str(evidence),
                   "--manifest", str(context["manifest_path"])]
        validated = run_stage("validate", command, context, report, 120)
        comparison = load_json((output / "validate.stdout.log").read_text())
        require(isinstance(comparison, dict) and comparison.get("status") in {"pass", "fail", "inconclusive", "blocked"},
                "Validator did not return a recognized strict comparison report")
        check_pins(context)
        require(comparison["status"] != "pass" or collected == validated == 0,
                "A subprocess failed despite a passing validator report")
        require((validated == 0) == (comparison["status"] == "pass"), "Validator exit code disagrees with its report")
        report["status"] = comparison["status"]
        report["comparison"] = comparison
        report["evidence"] = str(evidence)
        write_json(output / "comparison.json", comparison)
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        report["status"] = "blocked"
        report["reason"] = str(error)
    except KeyboardInterrupt:
        report["status"] = "blocked"
        report["reason"] = "Interrupted; retained partial outputs are not a passing result."
    finally:
        write_json(output / "local-run.json", report)
    return report


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--preflight-only", action="store_true", help="Check installed prerequisites only; no build, fetch or browser launch")
    parser.add_argument("--w3cos", type=Path, default=REPO / "target/release/w3cos", help="Existing release CLI (default: checkout target/release/w3cos)")
    parser.add_argument("--chromium", type=Path, help="Installed Chrome/Chromium executable; otherwise search Mac Applications and PATH")
    parser.add_argument("--lockfile", type=Path, default=HERE / "locks/native.Cargo.lock", help="Pinned generated native Cargo.lock (default: checked-in native lock)")
    parser.add_argument("--offline", action="store_true", help="No dependency fetch; builds use Cargo offline and require the exact pinned dependency cache")
    parser.add_argument("--output-directory", type=Path,
                        default=REPO.parent / (REPO.name + "-perf-results") / (datetime.now(timezone.utc).strftime("local-%Y%m%dT%H%M%SZ-") + uuid.uuid4().hex[:8]),
                        help="Fresh directory outside every Cargo workspace; default: ../CHECKOUT-perf-results/local-*")
    return parser.parse_args(argv)


def main(argv=None):
    args = arguments(argv)
    try:
        report, context = preflight(args)
        if report["status"] != "ready" or args.preflight_only:
            print(json.dumps(report, indent=2, allow_nan=False))
            return 0 if report["status"] == "ready" else 1
        print("Preflight ready. " + SCOPE + ". Keep the checkout unchanged and avoid competing workloads.\nRetaining outputs in "
              + str(context["output"]), file=sys.stderr, flush=True)
        report = execute(report, context, args.offline)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        report = {"status": "blocked", "reason": str(error), "scope": SCOPE}
    print(json.dumps(report, indent=2, allow_nan=False))
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
