#!/usr/bin/env python3
"""Build the observable-binding fixture in an independent generated project."""
import argparse, hashlib, json, os, shutil, subprocess
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument("--cli", type=Path, required=True)
p.add_argument("--output", type=Path, required=True)
p.add_argument("--lock", type=Path, required=True)
p.add_argument("--cache-target", type=Path)
a = p.parse_args()
source = Path(__file__).resolve().with_name("semantic-regressions.ts")
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
shim = out / "generate-only"
shim.mkdir()
(shim / "cargo").write_text("#!/bin/sh\n# Generation only; build the retained project below.\nexit 77\n")
(shim / "cargo").chmod(0o700)
env = dict(os.environ)
env.update(RUSTUP_AUTO_INSTALL="0", CARGO_NET_OFFLINE="true",
           W3COS_RELEASE_PROFILE="size", W3COS_RUNTIME_FEATURES="gpu")
for key in ["CARGO_TARGET_DIR", "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CARGO_PROFILE_RELEASE_OPT_LEVEL"]:
    env.pop(key, None)
generate_env = dict(env)
generate_env["PATH"] = str(shim) + os.pathsep + env["PATH"]
generate_env["TMPDIR"] = str(out)
r = subprocess.run([str(a.cli.resolve()), "build", str(source), "--release", "-o", str(out / "unused")],
                   env=generate_env, capture_output=True, text=True)
(out / "generate.log").write_text(r.stdout + r.stderr)
project = out / "w3cos-build"
if not (project / "src/esm_bundle.rs").exists():
    raise SystemExit("Fixture generation failed; see generate.log")
main = project / "src/main.rs"
text = main.read_text().replace("fn setup_dom() {", '''fn setup_dom() {
    // Test harness only: isolate a case without changing its lowering.
    if let Ok(selected) = std::env::var("W3COS_REVIEW_CASE") {
        w3cos_runtime::jsdom::window_value().set_property(
            "W3COS_REVIEW_CASE", w3cos_core::Value::string(&selected));
    }''', 1)
main.write_text(text)
shutil.copy2(a.lock, project / "Cargo.lock")
if a.cache_target:
    # macOS clonefile keeps the target independent while sharing immutable bytes.
    if os.uname().sysname == "Darwin":
        subprocess.run(["cp", "-cR", str(a.cache_target.resolve()), str(project / "target")], check=True)
    else:
        shutil.copytree(a.cache_target, project / "target")
for path in [main, project / "src/esm_bundle.rs", project / "Cargo.toml"]:
    path.touch()
with (out / "build.log").open("w") as log:
    subprocess.run(["cargo", "build", "--offline", "--locked", "--release"],
                   cwd=project, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
binary = out / "semantic-test"
shutil.copy2(project / "target/release/w3cos-app", binary)
proof = {"fixture_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
         "cli_sha256": hashlib.sha256(a.cli.read_bytes()).hexdigest(),
         "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
         "generated_sha256": {name: hashlib.sha256((project / name).read_bytes()).hexdigest()
                               for name in ["Cargo.toml", "Cargo.lock", "src/main.rs", "src/esm_bundle.rs"]}}
(out / "build-proof.json").write_text(json.dumps(proof, indent=2) + "\n")
print(json.dumps({"binary": str(binary), **proof}))
