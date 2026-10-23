#!/usr/bin/env python3
"""Fresh-process paired sampling. No builds or parallel benchmark processes."""
import argparse
import csv
import hashlib
import json
import os
import statistics
import subprocess
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument("baseline", type=Path)
p.add_argument("candidate", type=Path)
p.add_argument("output", type=Path)
p.add_argument("--pairs", type=int, default=5)
p.add_argument("--steady-iterations", type=int, default=20000)
p.add_argument("--controls-only", action="store_true")
p.add_argument("--lifecycle-controls", action="store_true")
p.add_argument("--composition-controls", action="store_true")
p.add_argument("--small-controls", action="store_true")
p.add_argument("--construction-controls", action="store_true")
p.add_argument("--registered-controls", action="store_true")
p.add_argument("--mixed-composition-controls", action="store_true")
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
bins = {"baseline": a.baseline.resolve(), "candidate": a.candidate.resolve()}
environment = {"pairs": a.pairs}
try:
    environment["effective_cpus"] = sorted(os.sched_getaffinity(0))
except (AttributeError, OSError) as error:
    environment["affinity_error"] = repr(error)
(a.output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
(a.output / "binaries.json").write_text(json.dumps({k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()} for k, v in bins.items()}, indent=2) + "\n")
cases = [(mode, count, a.steady_iterations) for mode in ["read", "method", "ordinary", "mixed"] for count in ([0, 1] if a.controls_only else [0, 1, 64, 1024])]
if not a.controls_only:
    cases += [("construct", count, 1024) for count in [0, 1024]]
    cases += [("churn", count, 16) for count in [64, 1024]]
if a.lifecycle_controls:
    cases = [("construct", n, 8192) for n in [0, 1024]] + [("lifecycle", n, 16) for n in [64, 1024]]
if a.composition_controls:
    cases = [(mode, n, 200000) for mode in ["read", "method", "ordinary", "mixed"] for n in [0, 1]] + [("mixed", n, 20000) for n in [64, 1024]]
if a.small_controls:
    cases = [(mode, n, 200000) for mode in ["read", "method", "ordinary", "mixed"] for n in [4, 16]]
if a.construction_controls:
    cases = [("construct", 1024, 8192)]
if a.registered_controls:
    cases = [("mixed", n, 20000) for n in [1, 4, 16, 64, 1024]]
if a.mixed_composition_controls:
    cases = [("mixed", n, 200000 if n in [0, 1] else 20000) for n in [0, 1, 64, 1024]]
rows = []
with (a.output / "raw.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["variant", "pair", "order", "mode", "views", "iterations", "nanoseconds", "checksum"])
    for mode, count, iterations in cases:
        for pair in range(a.pairs):
            order = ["baseline", "candidate"] if pair % 2 == 0 else ["candidate", "baseline"]
            results = []
            for position, variant in enumerate(order):
                run = subprocess.run([str(bins[variant]), mode, str(count), str(iterations)], text=True, capture_output=True, check=True)
                fields = next(csv.reader([run.stdout.strip()]))
                assert fields[:3] == [mode, str(count), str(iterations)], fields
                row = [variant, pair, position] + fields
                writer.writerow(row)
                out.flush()
                rows.append(row)
                results.append(fields[-1])
            assert results[0] == results[1], (mode, count, results)
        print(mode, count, "done", flush=True)
with (a.output / "summary.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["mode", "views", "iterations", "baseline_median_ns", "candidate_median_ns", "speedup", "baseline_min_ns", "baseline_max_ns", "candidate_min_ns", "candidate_max_ns", "paired_speedup_median", "paired_speedup_min", "paired_speedup_max", "candidate_faster_pairs", "pairs"])
    for mode, count, iterations in cases:
        values = {v: [int(r[6]) for r in rows if r[0] == v and r[3] == mode and int(r[4]) == count] for v in bins}
        b, c = (statistics.median(values[v]) for v in bins)
        ratios = [baseline / candidate for baseline, candidate in zip(values["baseline"], values["candidate"])]
        writer.writerow([mode, count, iterations, b, c, b / c, min(values["baseline"]), max(values["baseline"]), min(values["candidate"]), max(values["candidate"]), statistics.median(ratios), min(ratios), max(ratios), sum(r > 1 for r in ratios), len(ratios)])
