#!/usr/bin/env python3
"""Build the exact fixture with an existing w3cos CLI and retain provenance.
Does not install Rust, packages, or the CLI. Generated build files are retained.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import hashlib
import tomllib
import uuid

from validate import digest, file_digest, load_manifest, require


def source_snapshot(repo):
    names = subprocess.check_output(["git", "-C", str(repo), "ls-files", "--cached", "--others", "--exclude-standard", "-z"])
    snapshot = {}
    for name in names.split(b"\0"):
        if not name:
            continue
        relative = os.fsdecode(name)
        path = repo / relative
        require(not path.is_symlink(), "Source provenance does not support symlinked repository files")
        snapshot[relative] = file_digest(path) if path.is_file() else "missing"
    return snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--w3cos", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("manifest.json"))
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    output = args.output_directory.resolve()
    output.mkdir(parents=True, exist_ok=True)
    # A failed rebuild must not leave earlier success provenance usable. The
    # binary may remain for diagnostics, but this collector will reject it.
    (output / "build-provenance.json").write_text(json.dumps(
        {"schema": "w3cos-perf-native-build-v1", "status": "build-incomplete"}) + "\n")
    try:
        manifest = load_manifest(args.manifest)
        fixture = (args.manifest.parent / manifest["fixture"]).resolve()
        fixture_hash = file_digest(fixture)
        repo = args.repo_root.resolve()
        if output.is_relative_to(repo):
            ignored = subprocess.run(["git", "-C", str(repo), "check-ignore", "-q", str(output)], capture_output=True)
            require(ignored.returncode == 0, "Build output inside the repository must be git-ignored")
        initial_revision = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
        initial_snapshot = source_snapshot(repo)
        build_temp = output / ("compiler-temp-" + str(uuid.uuid4()))
        build_temp.mkdir()
        binary = output / "w3cos-dom-api-bench"
        env = dict(os.environ)
        env["TMPDIR"] = str(build_temp)
        # The CLI collects target/release/w3cos-app, and this contract is a
        # host-native build. Inherited cross-target/output overrides would
        # silently move the binary away from the CLI's collection path.
        env.pop("CARGO_TARGET_DIR", None)
        env.pop("CARGO_BUILD_TARGET", None)
        runtime_diff = subprocess.check_output(["git", "-C", str(args.repo_root), "diff", "HEAD", "--"] )
        command = [str(args.w3cos.resolve()), "build", str(fixture), "--release", "--output", str(binary)]
        # Retain partial diagnostics even if the build times out or is killed.
        # The selected checkout also supplies workspace discovery for a CLI
        # executable stored outside the repository.
        with (output / "build.log").open("w") as log:
            result = subprocess.run(command, cwd=repo, env=env, stdout=log,
                                    stderr=subprocess.STDOUT, text=True, timeout=1800)
        require(result.returncode == 0, "Native compilation failed; inspect build.log")
        generated = build_temp / "w3cos-build"
        entry = generated / "src/main.rs"
        bundle = generated / "src/esm_bundle.rs"
        require(bundle.is_file(), "Refusing legacy fallback: no ESM bundle generated")
        entry_text = entry.read_text()
        require("esm_bundle::run_entry_async" in entry_text and "ESM graph: unresolved" not in entry_text,
                "Refusing unresolved or legacy entry route")
        require("w3cos_runtime::jsdom" in bundle.read_text(), "ESM fixture did not bind real runtime DOM")
        generated_manifest = tomllib.loads((generated / "Cargo.toml").read_text())
        runtime_dependency = generated_manifest.get("dependencies", {}).get("w3cos-runtime", {})
        require(isinstance(runtime_dependency, dict) and "path" in runtime_dependency,
                "Generated native runtime source is not a local verified checkout")
        runtime_path = (generated / runtime_dependency["path"]).resolve()
        require(runtime_path == (args.repo_root / "crates/w3cos-runtime").resolve(),
                "CLI generated runtime dependency from a different checkout")
        require(file_digest(fixture) == fixture_hash, "Fixture changed during native build")
        require(runtime_diff == subprocess.check_output(["git", "-C", str(args.repo_root), "diff", "HEAD", "--"]),
                "Tracked runtime source changed during build")
        (output / "runtime-source.patch").write_bytes(runtime_diff)
        require(initial_snapshot == source_snapshot(repo), "Tracked or untracked repository source changed during build")
        (output / "source-snapshot.json").write_text(json.dumps(initial_snapshot, indent=2) + "\n")
        version = subprocess.check_output(["rustc", "--version"], text=True).strip()
        revision = subprocess.check_output(["git", "-C", str(args.repo_root), "rev-parse", "HEAD"], text=True).strip()
        require(revision == initial_revision, "Repository HEAD changed during build")
        provenance = {"schema": "w3cos-perf-native-build-v1", "route": "esm-aot-headless-release",
                      "fixture_sha256": fixture_hash, "binary_sha256": file_digest(binary),
                      "compiler_version": version, "runtime_git_revision": revision,
                      "runtime_diff_sha256": hashlib.sha256(runtime_diff).hexdigest(),
                      "source_snapshot_sha256": digest(initial_snapshot),
                      "cli_sha256": file_digest(args.w3cos), "generated_esm_sha256": file_digest(bundle),
                      "generated_main_sha256": file_digest(entry),
                      "lockfile_sha256": file_digest(generated / "Cargo.lock"), "build_command": command}
        (output / "build-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
        print(json.dumps({"status": "built", "binary": str(binary), "provenance": str(output / "build-provenance.json")}))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        (output / "build-error.json").write_text(json.dumps({"status": "blocked", "reason": str(error)}, indent=2) + "\n")
        print(json.dumps({"status": "blocked", "reason": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
