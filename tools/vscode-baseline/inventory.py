#!/usr/bin/env python3
"""Write the pinned Code-OSS dependency inventory. Does not compile Electron."""
import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path


class InventoryError(Exception):
    pass


def digest_bytes(data):
    return hashlib.sha256(data).hexdigest()


def load_json(path):
    return json.loads(Path(path).read_text())


def parse_npmrc(text):
    values = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or line.startswith(";"):
            continue
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def git_head(root):
    git = root / ".git"
    if not git.exists():
        return None
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=False, capture_output=True, text=True)
    if result.returncode != 0:
        raise InventoryError(result.stderr.strip() or "git rev-parse failed")
    return result.stdout.strip()


def classify(package, table):
    found = []
    dependencies = package.get("dependencies") or {}
    optional = package.get("optionalDependencies") or {}
    declared = dict(dependencies)
    declared.update(optional)
    by_name = {item["name"]: item for item in table["modules"]}
    for name in sorted(declared):
        spec = by_name.get(name)
        if spec is None:
            continue
        found.append({
            "name": name,
            "version": declared[name],
            "section": "optionalDependencies" if name in optional and name not in dependencies else "dependencies",
            "role": spec["role"],
            "plan": spec["plan"],
        })
    present = {item["name"] for item in found}
    absent = sorted(name for name in by_name if name not in present)
    return found, absent


def build_inventory(root, lock, allow_missing_git):
    root = Path(root)
    package_path = root / "package.json"
    npmrc_path = root / ".npmrc"
    product_path = root / "product.json"
    nvmrc_path = root / ".nvmrc"
    for path in (package_path, npmrc_path, product_path, nvmrc_path):
        if not path.is_file():
            raise InventoryError(f"missing {path}")
    package_bytes = package_path.read_bytes()
    npmrc_bytes = npmrc_path.read_bytes()
    product_bytes = product_path.read_bytes()
    nvmrc_bytes = nvmrc_path.read_bytes()
    package = json.loads(package_bytes)
    product = json.loads(product_bytes)
    npmrc = parse_npmrc(npmrc_bytes.decode())
    table_path = Path(__file__).with_name("native-modules.json")
    table = load_json(table_path)
    native, absent = classify(package, table)
    observed = git_head(root)
    if observed is None and not allow_missing_git:
        raise InventoryError(f"{root} has no .git; pass --allow-missing-git only for a fixture")
    problems = []
    expected_package = lock["package_json"]
    for key in ("name", "version", "main"):
        if package.get(key) != expected_package[key]:
            problems.append(f"package.json {key} is {package.get(key)!r}, lock has {expected_package[key]!r}")
    if digest_bytes(package_bytes) != expected_package["sha256"]:
        problems.append("package.json sha256 does not match the lock")
    for key in ("runtime", "target", "build_from_source"):
        if npmrc.get(key) != lock["npmrc"][key]:
            problems.append(f".npmrc {key} is {npmrc.get(key)!r}, lock has {lock['npmrc'][key]!r}")
    if digest_bytes(npmrc_bytes) != lock["npmrc"]["sha256"]:
        problems.append(".npmrc sha256 does not match the lock")
    expected_product = lock["product_json"]
    for key in ("applicationName", "nameShort"):
        if product.get(key) != expected_product[key]:
            problems.append(f"product.json {key} is {product.get(key)!r}, lock has {expected_product[key]!r}")
    if digest_bytes(product_bytes) != expected_product["sha256"]:
        problems.append("product.json sha256 does not match the lock")
    node = nvmrc_bytes.decode().strip()
    if node != lock["node"]:
        problems.append(f".nvmrc node is {node!r}, lock has {lock['node']!r}")
    if digest_bytes(nvmrc_bytes) != lock["nvmrc_sha256"]:
        problems.append(".nvmrc sha256 does not match the lock")
    if observed is not None and observed != lock["commit"]:
        problems.append(f"HEAD is {observed}, lock has {lock['commit']}")
    counts = {
        "dependencies": len(package.get("dependencies") or {}),
        "devDependencies": len(package.get("devDependencies") or {}),
        "optionalDependencies": len(package.get("optionalDependencies") or {}),
    }
    inventory = {
        "schema": "w3cos-code-oss-dependency-inventory-v1",
        "repository": lock["repository"],
        "lock_commit": lock["commit"],
        "observed_commit": observed,
        "commit_matches_lock": observed == lock["commit"] if observed is not None else None,
        "node": node,
        "package": {
            "name": package.get("name"),
            "version": package.get("version"),
            "main": package.get("main"),
            "sha256": digest_bytes(package_bytes),
        },
        "npmrc": {
            "runtime": npmrc.get("runtime"),
            "target": npmrc.get("target"),
            "build_from_source": npmrc.get("build_from_source"),
            "sha256": digest_bytes(npmrc_bytes),
        },
        "product": {
            "applicationName": product.get("applicationName"),
            "nameShort": product.get("nameShort"),
            "sha256": digest_bytes(product_bytes),
        },
        "dependency_counts": counts,
        "native_modules": native,
        "absent_native_modules": absent,
        "problems": problems,
    }
    return inventory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--lock", type=Path, default=Path(__file__).with_name("CODE_OSS.lock.json"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--allow-missing-git", action="store_true")
    args = parser.parse_args()
    lock = load_json(args.lock)
    if lock.get("schema") != "w3cos-code-oss-baseline-lock-v1":
        raise InventoryError("unexpected lock schema")
    try:
        inventory = build_inventory(args.root, lock, args.allow_missing_git)
    except InventoryError as error:
        print(str(error), file=sys.stderr)
        return 1
    text = json.dumps(inventory, indent=2) + "\n"
    if args.output:
        args.output.write_text(text)
    else:
        sys.stdout.write(text)
    if args.check and inventory["problems"]:
        print("\n".join(inventory["problems"]), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except InventoryError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
