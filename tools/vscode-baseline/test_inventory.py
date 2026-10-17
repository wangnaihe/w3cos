"""Lock checks for the Code-OSS dependency inventory. No Electron build."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import inventory


MINI_PACKAGE = {
    "name": "code-oss-dev",
    "version": "1.143.0",
    "main": "./out/main.js",
    "dependencies": {"node-pty": "1.2.0", "left-pad": "1.0.0"},
    "devDependencies": {"typescript": "5.0.0"},
}
MINI_PRODUCT = {"applicationName": "code-oss", "nameShort": "Code - OSS"}
MINI_NPMRC = 'runtime="electron"\ntarget="43.7.7"\nbuild_from_source="true"\n'
MINI_NODE = "24.18.0\n"


def write_tree(root, npmrc=MINI_NPMRC):
    root = Path(root)
    (root / "package.json").write_text(json.dumps(MINI_PACKAGE))
    (root / "product.json").write_text(json.dumps(MINI_PRODUCT))
    (root / ".npmrc").write_text(npmrc)
    (root / ".nvmrc").write_text(MINI_NODE)
    return root


def lock_for(root):
    root = Path(root)
    package = json.loads((root / "package.json").read_text())
    product = json.loads((root / "product.json").read_text())
    npmrc = inventory.parse_npmrc((root / ".npmrc").read_text())
    return {
        "schema": "w3cos-code-oss-baseline-lock-v1",
        "repository": "https://github.com/microsoft/vscode",
        "commit": "abc",
        "node": "24.18.0",
        "package_json": {
            "name": package["name"],
            "version": package["version"],
            "main": package["main"],
            "sha256": inventory.digest_bytes((root / "package.json").read_bytes()),
        },
        "npmrc": {
            "runtime": npmrc["runtime"],
            "target": npmrc["target"],
            "build_from_source": npmrc["build_from_source"],
            "sha256": inventory.digest_bytes((root / ".npmrc").read_bytes()),
        },
        "product_json": {
            "applicationName": product["applicationName"],
            "nameShort": product["nameShort"],
            "sha256": inventory.digest_bytes((root / "product.json").read_bytes()),
        },
        "nvmrc_sha256": inventory.digest_bytes((root / ".nvmrc").read_bytes()),
    }


class InventoryTests(unittest.TestCase):
    def test_matching_tree_lists_native_modules_and_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = write_tree(directory)
            report = inventory.build_inventory(root, lock_for(root), True)
        self.assertEqual(report["problems"], [])
        self.assertEqual(report["npmrc"]["target"], "43.7.7")
        self.assertEqual(report["dependency_counts"]["dependencies"], 2)
        names = [item["name"] for item in report["native_modules"]]
        self.assertEqual(names, ["node-pty"])
        self.assertIn("keytar", report["absent_native_modules"])
        self.assertNotIn("left-pad", names)

    def test_electron_target_mismatch_is_a_problem(self):
        with tempfile.TemporaryDirectory() as directory:
            root = write_tree(directory)
            lock = lock_for(root)
            (root / ".npmrc").write_text('runtime="electron"\ntarget="1.0.0"\nbuild_from_source="true"\n')
            report = inventory.build_inventory(root, lock, True)
        self.assertTrue(any("target" in problem for problem in report["problems"]))

    def test_check_cli_fails_on_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = write_tree(directory)
            lock_path = Path(directory) / "lock.json"
            lock_path.write_text(json.dumps(lock_for(root)))
            (root / ".nvmrc").write_text("20.0.0\n")
            result = subprocess.run(
                ["python3", str(Path(inventory.__file__)), "--root", directory,
                 "--lock", str(lock_path), "--check", "--allow-missing-git"],
                capture_output=True, text=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("node", result.stderr)


if __name__ == "__main__":
    unittest.main()
