#!/usr/bin/env python3
"""Fail-closed validation and per-scenario comparison of paired process runs."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import random
import statistics

METRIC = "synchronous-dom-script-wall-ms-v1"
RESET = "fresh-process-fixed-node-pools-v1"
CASES = ["append-remove-pool", "class-matches"]
PARAMETERS = {"nodes": 256, "rounds": 32, "warmup": 3, "samples": 7}
VIEWPORT = {"width": 1024, "height": 768, "dpr": 1}


class InvalidEvidence(ValueError):
    pass


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
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest()


def file_digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def numeric(value):
    return type(value) in (int, float) and math.isfinite(value)


def load_manifest(path):
    manifest = load_json(Path(path).read_text())
    # Version 1 deliberately supports only the authored matrix: changing a
    # workload or loosening a threshold requires an explicit new schema.
    expected = {
        "schema": "w3cos-paired-manifest-v1", "fixture": "scenarios.ts",
        "metric": METRIC, "reset": RESET, "parameters": PARAMETERS,
        "viewport": VIEWPORT, "scenarios": CASES, "pairs": 8,
        "native_ratio_limit": 0.95, "bootstrap_resamples": 10000,
        "family_alpha": 0.05, "random_seed": 0,
    }
    require(manifest == expected, "Unknown, incomplete, or modified v1 manifest")
    # Reject bools/numeric strings that Python equality could otherwise accept.
    require(digest(manifest) == digest(expected), "Manifest field types differ")
    return manifest


def validate_native_configuration(configuration, fixture_hash):
    # A rebuild embeds its exact predecessor. Validate the whole chain, not
    # just the final binary/lock hashes, without recursive schema dispatch.
    current, seen = configuration, set()
    while True:
        require(id(current) not in seen, "Cyclic native rebuild history")
        seen.add(id(current))
        _validate_native_configuration(current, fixture_hash)
        if "locked_rebuild" not in current:
            return
        metadata = current["locked_rebuild"]
        validate_locked_rebuild(current, metadata)
        current = metadata["previous_provenance"]


def _validate_native_configuration(configuration, fixture_hash):
    require(isinstance(configuration, dict), "Native provenance must be an object")
    require("status" not in configuration, "Incomplete or unsupported native build status")
    require(configuration.get("schema") == "w3cos-perf-native-build-v1", "Unknown native provenance schema")
    require(configuration.get("route") == "esm-aot-headless-release", "Unsupported native compilation route")
    require(configuration.get("fixture_sha256") == fixture_hash, "Native build source mismatch")
    for field in ["binary_sha256", "lockfile_sha256", "generated_esm_sha256", "cli_sha256",
                  "generated_main_sha256", "runtime_diff_sha256", "source_snapshot_sha256"]:
        value = configuration.get(field)
        require(isinstance(value, str) and len(value) == 64
                and all(c in "0123456789abcdef" for c in value), "Missing/invalid native digest: " + field)
    version = configuration.get("compiler_version")
    require(isinstance(version, str) and version.strip(), "Missing native compiler version")
    revision = configuration.get("runtime_git_revision")
    require(isinstance(revision, str) and len(revision) in (40, 64)
            and all(c in "0123456789abcdef" for c in revision), "Missing/invalid runtime revision")
    command = configuration.get("build_command")
    require(isinstance(command, list) and len(command) == 6
            and all(isinstance(item, str) and item for item in command)
            and command[1] == "build" and command[3:5] == ["--release", "--output"],
            "Missing release build command")


def validate_locked_rebuild(configuration, metadata):
    """Validate optional locked-rebuild metadata; legacy successful v1 stays valid."""
    fields = {"schema", "previous_provenance", "previous_provenance_sha256",
              "repo_root", "generated_project", "generated_inputs", "lockfile_source",
              "lockfile_sha256", "command", "working_directory", "cargo_sha256",
              "cargo_version", "history_directory", "build_log_sha256", "binary_sha256"}
    require(isinstance(metadata, dict) and set(metadata) == fields, "Malformed locked rebuild metadata")
    require(metadata["schema"] == "w3cos-perf-locked-rebuild-v1", "Unknown locked rebuild schema")
    previous = metadata["previous_provenance"]
    require(isinstance(previous, dict), "Missing original rebuild provenance")
    for field in ["previous_provenance_sha256", "lockfile_sha256", "cargo_sha256",
                  "build_log_sha256", "binary_sha256"]:
        value = metadata[field]
        require(isinstance(value, str) and len(value) == 64
                and all(c in "0123456789abcdef" for c in value), "Invalid rebuild digest: " + field)
    try:
        previous_hash = digest(previous)
    except (ValueError, TypeError, RecursionError) as error:
        raise InvalidEvidence("Malformed previous rebuild provenance") from error
    require(previous_hash == metadata["previous_provenance_sha256"], "Rebuild predecessor digest mismatch")
    unchanged = lambda value: {key: item for key, item in value.items()
                               if key not in {"binary_sha256", "lockfile_sha256", "locked_rebuild"}}
    require(unchanged(configuration) == unchanged(previous), "Rebuild changed original source/build identity")
    for field in ["binary_sha256", "lockfile_sha256"]:
        require(metadata[field] == configuration[field], "Rebuild final digest mismatch: " + field)
    for field in ["repo_root", "generated_project", "lockfile_source", "working_directory", "history_directory"]:
        value = metadata[field]
        require(isinstance(value, str) and Path(value).is_absolute()
                and ".." not in Path(value).parts and str(Path(value)) == value,
                "Invalid rebuild path: " + field)
    require(metadata["working_directory"] == metadata["generated_project"], "Wrong rebuild working directory")
    project = Path(metadata["generated_project"])
    output = Path(configuration["build_command"][5]).parent
    history = Path(metadata["history_directory"])
    require(output.is_absolute() and project.name == "w3cos-build"
            and project.parent.name.startswith("compiler-temp-") and project.parent.parent == output,
            "Rebuild project is outside the retained build directory")
    require(history.parent.name == "locked-rebuild-history" and history.parent.parent == output,
            "Rebuild history is outside the retained build directory")
    require(Path(configuration["build_command"][2]) == Path(metadata["repo_root"]) / "tools/perf-paired/scenarios.ts",
            "Rebuild checkout differs from original fixture checkout")
    require(Path(metadata["lockfile_source"]) != project / "Cargo.lock", "Rebuild lock input is not separately pinned")
    command = metadata["command"]
    require(isinstance(command, list) and len(command) == 5
            and isinstance(command[0], str) and Path(command[0]).is_absolute()
            and command[1:] == ["build", "--release", "--locked", "--offline"],
            "Rebuild must use offline locked release Cargo command")
    require(isinstance(metadata["cargo_version"], str) and metadata["cargo_version"].strip(),
            "Missing rebuild Cargo version")
    inputs = metadata["generated_inputs"]
    require(isinstance(inputs, dict) and set(inputs) == {"Cargo.toml", "src/main.rs", "src/esm_bundle.rs"},
            "Incomplete generated rebuild inputs")
    for value in inputs.values():
        require(isinstance(value, str) and len(value) == 64
                and all(c in "0123456789abcdef" for c in value), "Invalid generated rebuild digest")
    require(inputs["src/main.rs"] == configuration["generated_main_sha256"]
            and inputs["src/esm_bundle.rs"] == configuration["generated_esm_sha256"],
            "Rebuild changed generated fixture source")
    if "locked_rebuild" in previous:
        old = previous["locked_rebuild"]
        require(isinstance(old, dict), "Malformed previous rebuild metadata")
        for field in ["repo_root", "generated_project", "generated_inputs"]:
            require(metadata[field] == old.get(field), "Rebuild history source drift: " + field)
        require(metadata["history_directory"] != old.get("history_directory"), "Rebuild reused a history directory")


def validate_browser_configuration(configuration):
    require(isinstance(configuration, dict), "Browser configuration must be an object")
    for field in ["sandbox_enabled", "offline", "headless"]:
        require(configuration.get(field) is True, "Unsupported browser setting: " + field)
    require(configuration.get("viewport") == VIEWPORT, "Browser configuration viewport mismatch")
    require(all(numeric(value) and value > 0 for value in configuration["viewport"].values()),
            "Invalid browser configuration viewport types")
    require(configuration.get("service_workers") == "block", "Unsupported service-worker policy")
    require(configuration.get("ignored_default_args") == ["--unsafely-disable-devtools-self-xss-warnings", "--enable-unsafe-swiftshader"],
            "Unsupported browser launch argument policy")
    for field in ["chromium_version", "playwright_version"]:
        require(isinstance(configuration.get(field), str) and configuration[field].strip(), "Missing browser version metadata")
    value = configuration.get("executable_sha256")
    require(isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value),
            "Missing browser executable identity")


def validate_result(result, manifest):
    require(isinstance(result, dict), "Fixture result must be an object")
    require(result.get("schema") == "w3cos-dom-api-v1", "Unknown fixture schema")
    require(result.get("status") == "ok", "Fixture did not report correctness success")
    require(result.get("metric") == METRIC, "Mismatched timing semantics or units")
    require(result.get("reset") == RESET, "Mismatched reset protocol")
    require(result.get("parameters") == PARAMETERS, "Mismatched workload or warmup")
    require(digest(result["parameters"]) == digest(PARAMETERS), "Invalid parameter types")
    require(result.get("viewport") == VIEWPORT, "Mismatched observed viewport/DPR")
    require(all(numeric(v) and v > 0 for v in result["viewport"].values()), "Invalid viewport values")
    cases = result.get("scenarios")
    require(isinstance(cases, list) and len(cases) == len(CASES)
            and all(isinstance(case, dict) for case in cases), "Missing or malformed scenarios")
    require([case.get("id") for case in cases if isinstance(case, dict)] == CASES,
            "Missing, extra, duplicate, or reordered scenario")
    medians = {}
    n, rounds = PARAMETERS["nodes"], PARAMETERS["rounds"]
    for case in cases:
        samples = case.get("samples")
        require(isinstance(samples, list) and len(samples) == PARAMETERS["samples"], "Wrong measured sample count")
        values = []
        for index, sample in enumerate(samples):
            require(isinstance(sample, dict), "Sample must be an object")
            require(sample.get("phase") == "measurement" and type(sample.get("index")) is int
                    and sample["index"] == index, "Warmup, missing, or reordered sample")
            value = sample.get("elapsed_ms")
            require(numeric(value) and value > 0, "Duration must be finite positive milliseconds, not bool/string")
            expected = ({"checks": rounds * (5 * n + 2), "final_children": 0}
                        if case["id"] == "append-remove-pool" else
                        {"checks": 3 * n * rounds, "true_matches": n * rounds,
                         "false_matches": n * rounds, "final_class": "bench inactive"})
            require(sample.get("evidence") == expected, "Incorrect transient/final-state evidence")
            require(digest(sample["evidence"]) == digest(expected), "Incorrect evidence field types")
            values.append(value)
        medians[case["id"]] = statistics.median(values)
    return medians


def validate_envelope(envelope, manifest, fixture_hash):
    require(isinstance(envelope, dict), "Evidence must be an object")
    require(envelope.get("schema") == "w3cos-paired-evidence-v1", "Unknown evidence schema")
    require(envelope.get("collection_status") == "complete", "Collection blocked, incomplete, or browser-only")
    require(envelope.get("manifest_sha256") == digest(manifest), "Manifest digest mismatch")
    require(envelope.get("fixture_sha256") == fixture_hash, "Fixture digest mismatch")
    host = envelope.get("host_session")
    require(isinstance(host, str) and len(host) >= 16, "Missing common host/session identity")
    runs = envelope.get("runs")
    require(isinstance(runs, list) and len(runs) == 2 * manifest["pairs"], "Missing native/browser arm or pair")
    seen_ids, seen_results = set(), set()
    versions, configurations, values = {}, {}, {case: [] for case in CASES}
    native_build = envelope.get("native_build")
    validate_native_configuration(native_build, fixture_hash)
    pair_values = {}
    for index, run in enumerate(runs):
        require(isinstance(run, dict), "Run must be an object")
        pair = index // 2
        expected_engine = (["native", "browser"] if pair % 2 == 0 else ["browser", "native"])[index % 2]
        require(type(run.get("pair")) is int and run["pair"] == pair, "Wrong or duplicate pair id")
        require(type(run.get("order")) is int and run["order"] == index, "Wrong execution order")
        require(run.get("engine") == expected_engine, "Runs are not alternating paired arms")
        run_id = run.get("run_id")
        require(isinstance(run_id, str) and run_id and run_id not in seen_ids, "Missing or duplicate run id")
        seen_ids.add(run_id)
        require(run.get("host_session") == host, "Runs did not share one measurement host/session")
        require(run.get("fixture_sha256") == fixture_hash, "Run used a different fixture")
        mode = "esm-aot-headless-release" if expected_engine == "native" else "chromium-headless"
        require(run.get("execution_mode") == mode, "Unsupported execution/build mode")
        version = run.get("engine_version")
        require(isinstance(version, str) and version.strip(), "Missing engine version/build identity")
        require(expected_engine not in versions or versions[expected_engine] == version, "Engine changed during measurement")
        versions[expected_engine] = version
        configuration = run.get("engine_configuration")
        require(isinstance(configuration, dict) and configuration, "Missing engine configuration")
        config_hash = digest(configuration)
        require(run.get("configuration_sha256") == config_hash, "Engine configuration digest mismatch")
        require(expected_engine not in configurations or configurations[expected_engine] == config_hash,
                "Build or engine configuration changed during measurement")
        configurations[expected_engine] = config_hash
        if expected_engine == "native":
            require(configuration == native_build, "Native run differs from retained build provenance")
            require(version == configuration["runtime_git_revision"] + "; rustc=" + configuration["compiler_version"],
                    "Native engine version differs from retained provenance")
        else:
            validate_browser_configuration(configuration)
            require(version == configuration["chromium_version"] + "; playwright=" + configuration["playwright_version"],
                    "Browser engine version differs from retained configuration")
        result = run.get("result")
        medians = validate_result(result, manifest)
        result_hash = digest(result)
        require(run.get("result_sha256") == result_hash, "Raw result digest mismatch")
        key = (expected_engine, result_hash)
        require(key not in seen_results, "Duplicate/reused result; collect a fresh process run")
        seen_results.add(key)
        pair_values[(pair, expected_engine)] = medians
    for case in CASES:
        for pair in range(manifest["pairs"]):
            ratio = pair_values[(pair, "native")][case] / pair_values[(pair, "browser")][case]
            require(numeric(ratio) and ratio > 0, "Ratio overflow/underflow")
            values[case].append(ratio)
    return values


def compare(envelope, manifest, fixture_hash):
    try:
        ratios = validate_envelope(envelope, manifest, fixture_hash)
    except (InvalidEvidence, KeyError, TypeError, ValueError, OverflowError) as error:
        return {"status": "blocked", "reason": str(error), "scenarios": []}
    reports = []
    for name in CASES:
        # The process pair, not each within-process sample, is the resampling unit.
        logs = [math.log(ratio) for ratio in ratios[name]]
        rng = random.Random(manifest["random_seed"])
        bootstrap = sorted(math.exp(statistics.mean(rng.choices(logs, k=len(logs))))
                           for _ in range(manifest["bootstrap_resamples"]))
        # Bonferroni-adjusted two-sided percentile intervals across this fixed matrix.
        tail = manifest["family_alpha"] / (2 * len(CASES))
        low = bootstrap[int(tail * (len(bootstrap) - 1))]
        high = bootstrap[math.ceil((1 - tail) * (len(bootstrap) - 1))]
        if high < manifest["native_ratio_limit"] and max(ratios[name]) < 1:
            status = "pass"
        elif low > 1:
            status = "fail"
        else:
            status = "inconclusive"
        reports.append({"id": name, "status": status, "paired_native_browser_ratios": ratios[name],
                        "geometric_mean_ratio": math.exp(statistics.mean(logs)),
                        "bootstrap_lower": low, "bootstrap_upper": high})
    overall = ("pass" if all(item["status"] == "pass" for item in reports) else
               "fail" if any(item["status"] == "fail" for item in reports) else "inconclusive")
    return {"status": overall, "scope": "Only the two declared synchronous DOM API scenarios; not frame/display/app performance",
            "statistical_caveat": "Bootstrap intervals are estimates from eight process pairs, not universal guarantees",
            "scenarios": reports}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("manifest.json"))
    args = parser.parse_args()
    try:
        manifest = load_manifest(args.manifest)
        report = compare(load_json(args.evidence.read_text()), manifest,
                         file_digest(args.manifest.parent / manifest["fixture"]))
    except (OSError, ValueError, TypeError) as error:
        report = {"status": "blocked", "reason": str(error)}
    print(json.dumps(report, indent=2, allow_nan=False))
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
