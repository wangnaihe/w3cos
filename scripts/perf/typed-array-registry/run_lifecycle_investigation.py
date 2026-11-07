#!/usr/bin/env python3
"""One bounded three-way lifecycle/constructor repeat, preserving every sample."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess

p = argparse.ArgumentParser()
p.add_argument("main", type=Path)
p.add_argument("initial", type=Path)
p.add_argument("fastpath", type=Path)
p.add_argument("output", type=Path)
p.add_argument("--rounds", type=int, default=11)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
bins = {name: getattr(a, name).resolve() for name in ["main", "initial", "fastpath"]}
metadata = {"rounds": a.rounds, "binaries": {k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()} for k, v in bins.items()}}
try:
    allowed = sorted(os.sched_getaffinity(0))
    os.sched_setaffinity(0, {allowed[0]})
    metadata.update(allowed_cpus=allowed, effective_cpus=sorted(os.sched_getaffinity(0)))
except (AttributeError, OSError) as error:
    metadata["affinity_error"] = repr(error)
metadata["variant_order"] = "rotate ABC, BCA, CAB, then reverse CBA, BAC, ACB; repeat"
metadata["case_order"] = "construct/lifecycle on even rounds, reversed on odd rounds"
(a.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
rows = []
cases = [("construct", 1024, 8192), ("lifecycle", 1024, 16)]
with (a.output / "raw.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["variant", "round", "case_order", "variant_order", "mode", "views", "iterations", "nanoseconds", "checksum"])
    for round_number in range(a.rounds):
        order = list(bins) if (round_number // 3) % 2 == 0 else list(reversed(bins))
        rotation = round_number % 3
        order = order[rotation:] + order[:rotation]
        case_order = cases if round_number % 2 == 0 else list(reversed(cases))
        for case_position, (mode, count, iterations) in enumerate(case_order):
            checksums = []
            for position, variant in enumerate(order):
                result = subprocess.run([str(bins[variant]), mode, str(count), str(iterations)], text=True, capture_output=True, check=True)
                fields = next(csv.reader([result.stdout.strip()]))
                assert fields[:3] == [mode, str(count), str(iterations)]
                writer.writerow([variant, round_number, case_position, position] + fields)
                out.flush()
                rows.append((mode, round_number, variant, int(fields[3])))
                checksums.append(fields[4])
            assert len(set(checksums)) == 1
        print("round", round_number, "done", flush=True)
with (a.output / "variant-summary.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["mode", "variant", "median_ns", "min_ns", "max_ns", "rounds"])
    for mode, _, _ in cases:
        for variant in bins:
            ns = [ns for m, _, v, ns in rows if m == mode and v == variant]
            writer.writerow([mode, variant, statistics.median(ns), min(ns), max(ns), len(ns)])
with (a.output / "paired.csv").open("w", newline="") as out, (a.output / "paired-summary.csv").open("w", newline="") as summary:
    writer, summarizer = csv.writer(out), csv.writer(summary)
    writer.writerow(["mode", "round", "reference", "candidate", "reference_ns", "candidate_ns", "speedup", "candidate_minus_reference_ns"])
    summarizer.writerow(["mode", "reference", "candidate", "paired_speedup_median", "paired_speedup_min", "paired_speedup_max", "candidate_faster_rounds", "rounds", "paired_difference_median_ns"])
    for mode, _, _ in cases:
        for reference, candidate in [("main", "initial"), ("main", "fastpath"), ("initial", "fastpath")]:
            ratios, differences = [], []
            for round_number in range(a.rounds):
                values = {v: ns for m, r, v, ns in rows if m == mode and r == round_number}
                b, c = values[reference], values[candidate]
                ratios.append(b / c)
                differences.append(c - b)
                writer.writerow([mode, round_number, reference, candidate, b, c, b / c, c - b])
            summarizer.writerow([mode, reference, candidate, statistics.median(ratios), min(ratios), max(ratios), sum(r > 1 for r in ratios), len(ratios), statistics.median(differences)])
