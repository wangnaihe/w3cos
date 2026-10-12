#!/usr/bin/env python3
"""Rebuild a verified retained native project with an explicitly supplied lockfile.

Does not invoke the CLI, regenerate sources, download packages, or run the binary.
The original CLI command stays in provenance; a separate locked_rebuild record
describes the actual Cargo invocation and embeds the exact previous provenance.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib
import uuid

from build_native import source_snapshot
from validate import digest, file_digest, load_json, load_manifest, require, validate_native_configuration

GENERATED_FILES = {"Cargo.toml", "src/main.rs", "src/esm_bundle.rs"}


def regular_path(path, directory=False):
    """Reject symlinks in every path component before resolving a retained input."""
    path = Path(path).absolute()
    require(all(not parent.is_symlink() for parent in (path, *path.parents)),
            "Symlinked retained input is unsupported: " + str(path))
    require(path.is_dir() if directory else path.is_file(), "Missing retained input: " + str(path))
    return path.resolve()


def atomic_json(path, value):
    temporary = path.with_name(path.name + "." + str(uuid.uuid4()) + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")
    temporary.replace(path)


def expected_manifest(repo):
    # This is the authored fixture's generated ESM/native shape, not a generic
    # Cargo manifest allowlist. Old v1 provenance did not hash Cargo.toml, so
    # unsupported dependencies, build scripts, targets or profiles fail closed.
    return {
        "package": {"name": "w3cos-app", "version": "0.1.0", "edition": "2024"},
        "dependencies": {
            "w3cos-core": {"path": str(repo / "crates/w3cos-core")},
            "w3cos-runtime": {"path": str(repo / "crates/w3cos-runtime"),
                              "default-features": False, "features": ["gpu"]},
            "w3cos-std": {"path": str(repo / "crates/w3cos-std")}},
        "profile": {"release": {"opt-level": "z", "lto": "fat", "codegen-units": 1,
                                "panic": "unwind", "strip": "symbols"}}}


def generated_inputs(project, repo):
    require({path.name for path in project.iterdir()} <= {"Cargo.toml", "Cargo.lock", "src", "target"},
            "Unrecorded generated project input")
    regular_path(project / "src", directory=True)
    require({path.name for path in (project / "src").iterdir()} == {"main.rs", "esm_bundle.rs"},
            "Unrecorded generated source input")
    if (project / "target").exists() or (project / "target").is_symlink():
        target = regular_path(project / "target", directory=True)
        # Cargo may write through existing output directories even when the
        # final executable is absent. Reject those aliases before invoking it.
        for root, directories, files in os.walk(target):
            require(not any((Path(root) / name).is_symlink() for name in directories + files),
                    "Symlinked generated Cargo output is unsupported")
    result = {name: file_digest(regular_path(project / name)) for name in sorted(GENERATED_FILES)}
    manifest = tomllib.loads((project / "Cargo.toml").read_text())
    require(digest(manifest) == digest(expected_manifest(repo)),
            "Generated manifest differs from supported fixture/check-out/profile")
    entry, bundle = (project / "src/main.rs").read_text(), (project / "src/esm_bundle.rs").read_text()
    require("esm_bundle::run_entry_async" in entry and "ESM graph: unresolved" not in entry,
            "Refusing unresolved or legacy entry route")
    require("w3cos_runtime::jsdom" in bundle, "Generated fixture does not bind real runtime DOM")
    return result


def check_build_environment(project, environment):
    # Do not silently build with flags/toolchain wrappers absent from old v1
    # provenance. Ordinary dependency/toolchain search paths remain inherited.
    overrides = {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTC_WRAPPER",
                 "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC_WRAPPER",
                 "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTFLAGS"}
    require(not any(value and (key in overrides or key.startswith("CARGO_PROFILE_")
                    or key.startswith("CARGO_TARGET_") and key != "CARGO_TARGET_DIR")
                    for key, value in environment.items()), "Unrecorded compiler/profile/target override")
    homes = [parent / ".cargo" for parent in (project, *project.parents)]
    homes.append(Path(environment.get("CARGO_HOME", str(Path.home() / ".cargo"))))
    for home in homes:
        for name in ("config", "config.toml"):
            require(not (home / name).exists() and not (home / name).is_symlink(),
                    "Unrecorded Cargo configuration: " + str(home / name))
    for parent in (project, *project.parents):
        for name in ("rust-toolchain", "rust-toolchain.toml"):
            require(not (parent / name).exists() and not (parent / name).is_symlink(),
                    "Unrecorded toolchain override: " + str(parent / name))
    # rustup resolves overrides using cwd; always probe inside the same project
    # that Cargo will build, rather than the helper's own checkout.


def verify_checkout(repo, original, snapshot, patch_bytes, fixture, cli, project, environment):
    require(subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
            == original["runtime_git_revision"], "Repository HEAD differs from original build")
    require(subprocess.check_output(["git", "-C", str(repo), "diff", "HEAD", "--"]) == patch_bytes,
            "Tracked source differs from original build")
    require(source_snapshot(repo) == snapshot, "Tracked or untracked source differs from original build")
    require(file_digest(fixture) == original["fixture_sha256"], "Fixture differs from original build")
    require(file_digest(cli) == original["cli_sha256"], "CLI differs from original build")
    require(subprocess.check_output(["rustc", "--version"], cwd=project, env=environment, text=True).strip()
            == original["compiler_version"], "Compiler differs from original build")


def rebuild(args):
    output = regular_path(args.build_directory, directory=True)
    repo = regular_path(args.repo_root, directory=True)
    if output.is_relative_to(repo):
        ignored = subprocess.run(["git", "-C", str(repo), "check-ignore", "-q", str(output)], capture_output=True)
        require(ignored.returncode == 0, "Build output inside the repository must be git-ignored")
    manifest_path = regular_path(repo / "tools/perf-paired/manifest.json")
    manifest = load_manifest(manifest_path)
    fixture = regular_path(manifest_path.parent / manifest["fixture"])
    provenance_path = regular_path(output / "build-provenance.json")
    provenance_bytes = provenance_path.read_bytes()
    original = load_json(provenance_bytes.decode())
    validate_native_configuration(original, file_digest(fixture))
    binary = regular_path(output / "w3cos-dom-api-bench")
    require(file_digest(binary) == original["binary_sha256"], "Retained binary differs from provenance")
    command = original["build_command"]
    require(Path(command[2]).resolve() == fixture and Path(command[5]).resolve() == binary,
            "Original CLI command identifies a different fixture/output")
    cli = regular_path(command[0])
    snapshot_path = regular_path(output / "source-snapshot.json")
    snapshot_bytes = snapshot_path.read_bytes()
    snapshot = load_json(snapshot_bytes.decode())
    require(isinstance(snapshot, dict) and digest(snapshot) == original["source_snapshot_sha256"],
            "Retained source snapshot differs from provenance")
    patch_path = regular_path(output / "runtime-source.patch")
    patch_bytes = patch_path.read_bytes()
    require(hashlib.sha256(patch_bytes).hexdigest() == original["runtime_diff_sha256"],
            "Retained source patch differs from provenance")
    # Multiple old compiler temp directories are permitted only if exactly one
    # has the source AND lock hashes of the most recent successful provenance.
    projects = []
    for path in output.glob("compiler-temp-*/w3cos-build"):
        project = regular_path(path, directory=True)
        if all((project / name).is_file() and file_digest(regular_path(project / name)) == original[field]
               for name, field in [("src/main.rs", "generated_main_sha256"),
                                   ("src/esm_bundle.rs", "generated_esm_sha256"), ("Cargo.lock", "lockfile_sha256")]):
            projects.append(project)
    require(len(projects) == 1, "Need exactly one retained generated project matching successful provenance")
    project = projects[0]
    inputs = generated_inputs(project, repo)
    if "locked_rebuild" in original:
        previous = original["locked_rebuild"]
        require(previous["repo_root"] == str(repo) and previous["generated_project"] == str(project)
                and previous["generated_inputs"] == inputs, "Retained rebuild source history drift")
    pinned = regular_path(args.lockfile)
    require(pinned != project / "Cargo.lock", "Supply a separate, explicitly pinned baseline lockfile")
    pinned_bytes = pinned.read_bytes()
    lock = tomllib.loads(pinned_bytes.decode())
    require(type(lock.get("version")) is int and lock["version"] in (3, 4)
            and isinstance(lock.get("package"), list) and lock["package"], "Unsupported or empty pinned Cargo.lock")
    pinned_hash = hashlib.sha256(pinned_bytes).hexdigest()
    environment = dict(os.environ)
    environment.pop("CARGO_TARGET_DIR", None)
    environment.pop("CARGO_BUILD_TARGET", None)
    environment["CARGO_NET_OFFLINE"] = "true"
    environment["CARGO_TARGET_DIR"] = str(project / "target")
    check_build_environment(project, environment)
    cargo_name = str(args.cargo) if args.cargo else shutil.which("cargo", path=environment.get("PATH"))
    require(cargo_name, "An installed Cargo is required")
    # Keep the executable's symlink name: resolving a rustup proxy to `rustup`
    # would change argv[0] and therefore which tool it dispatches.
    cargo = Path(cargo_name).absolute()
    cargo_hash = file_digest(cargo)
    cargo_version = subprocess.check_output([str(cargo), "--version"], cwd=project, env=environment, text=True).strip()
    verify_checkout(repo, original, snapshot, patch_bytes, fixture, cli, project, environment)

    history = None
    # Exclusive creation prevents two instances of THIS helper sharing output.
    # Callers must still exclude external source edits/CLI/Cargo invocations.
    lease = output / ".locked-rebuild-in-progress"
    with lease.open("x") as handle:
        handle.write(str(os.getpid()) + "\n")
    try:
        require(provenance_path.read_bytes() == provenance_bytes, "Provenance changed during rebuild preflight")
        history_root = output / "locked-rebuild-history"
        if history_root.exists() or history_root.is_symlink():
            regular_path(history_root, directory=True)
        else:
            history_root.mkdir()
        history = history_root / str(uuid.uuid4())
        history.mkdir()
        # Preserve original bytes and binary before invalidating success. Never
        # overwrite the original CLI build log or any earlier history directory.
        (history / "build-provenance.json").write_bytes(provenance_bytes)
        (history / "source-snapshot.json").write_bytes(snapshot_bytes)
        (history / "runtime-source.patch").write_bytes(patch_bytes)
        shutil.copy2(binary, history / binary.name)
        shutil.copy2(project / "Cargo.lock", history / "Cargo.lock")
        shutil.copy2(project / "Cargo.toml", history / "Cargo.toml")
        (history / "pinned-Cargo.lock").write_bytes(pinned_bytes)
        require(file_digest(history / binary.name) == original["binary_sha256"]
                and file_digest(history / "Cargo.lock") == original["lockfile_sha256"],
                "Original build changed while archiving")
        incomplete = {"schema": original["schema"], "status": "locked-rebuild-incomplete",
                      "history_directory": str(history)}
        atomic_json(provenance_path, incomplete)
        build_command = [str(cargo), "build", "--release", "--locked", "--offline"]
        atomic_json(history / "attempt.json", {"command": build_command, "working_directory": str(project),
                                               "lockfile_source": str(pinned), "lockfile_sha256": pinned_hash})
        require(generated_inputs(project, repo) == inputs and pinned.read_bytes() == pinned_bytes,
                "Rebuild inputs changed while archiving")
        verify_checkout(repo, original, snapshot, patch_bytes, fixture, cli, project, environment)
        (project / "Cargo.lock").write_bytes(pinned_bytes)
        built = project / "target/release/w3cos-app"
        if built.exists() or built.is_symlink():
            # Cargo must produce an executable in this invocation; a stale
            # target file cannot stand in for a missing rebuild output.
            regular_path(built).replace(history / "previous-target-w3cos-app")
        log_path = history / "rebuild.log"
        with log_path.open("w") as log:
            result = subprocess.run(build_command, cwd=project, env=environment, stdout=log,
                                    stderr=subprocess.STDOUT, text=True, timeout=1800)
        require(result.returncode == 0, "Locked native rebuild failed; inspect " + str(log_path))
        require(generated_inputs(project, repo) == inputs, "Generated inputs changed during locked rebuild")
        require(file_digest(project / "Cargo.lock") == pinned_hash and pinned.read_bytes() == pinned_bytes,
                "Pinned lockfile changed during locked rebuild")
        require(snapshot_path.read_bytes() == snapshot_bytes and patch_path.read_bytes() == patch_bytes,
                "Retained source records changed during locked rebuild")
        require(file_digest(binary) == original["binary_sha256"], "Retained binary changed during locked rebuild")
        require(file_digest(cargo) == cargo_hash, "Cargo executable changed during locked rebuild")
        require(load_json(provenance_path.read_text()) == incomplete,
                "Active provenance changed during locked rebuild")
        check_build_environment(project, environment)
        verify_checkout(repo, original, snapshot, patch_bytes, fixture, cli, project, environment)
        built = regular_path(built)
        require(os.access(built, os.X_OK), "Cargo output is not an executable")
        built_hash = file_digest(built)
        final = {**original, "binary_sha256": built_hash, "lockfile_sha256": pinned_hash,
                 "locked_rebuild": {
                     "schema": "w3cos-perf-locked-rebuild-v1", "previous_provenance": original,
                     "previous_provenance_sha256": digest(original), "repo_root": str(repo),
                     "generated_project": str(project), "generated_inputs": inputs,
                     "lockfile_source": str(pinned), "lockfile_sha256": pinned_hash,
                     "command": build_command, "working_directory": str(project),
                     "cargo_sha256": cargo_hash, "cargo_version": cargo_version,
                     "history_directory": str(history), "build_log_sha256": file_digest(log_path),
                     "binary_sha256": built_hash}}
        validate_native_configuration(final, original["fixture_sha256"])
        # Publish binary first while provenance is unusable, provenance last.
        temporary = output / ("rebuilt-binary-" + str(uuid.uuid4()))
        shutil.copy2(built, temporary)
        require(file_digest(temporary) == built_hash, "Rebuilt executable changed while copying")
        temporary.replace(binary)
        atomic_json(history / "result-provenance.json", final)
        atomic_json(provenance_path, final)
        return {"status": "rebuilt", "binary": str(binary), "provenance": str(provenance_path),
                "history_directory": str(history)}
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        if history is not None:
            atomic_json(history / "error.json", {"status": "blocked", "reason": str(error)})
        raise
    finally:
        lease.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-directory", type=Path, required=True)
    parser.add_argument("--repo-root", type=Path, required=True)
    parser.add_argument("--lockfile", type=Path, required=True)
    parser.add_argument("--cargo", type=Path, help="Installed Cargo executable; defaults to PATH")
    args = parser.parse_args()
    try:
        print(json.dumps(rebuild(args)))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(json.dumps({"status": "blocked", "reason": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
