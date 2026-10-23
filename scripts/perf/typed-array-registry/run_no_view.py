#!/usr/bin/env python3
"""Bounded, interleaved no-view control on one permitted CPU when supported."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess

p = argparse.ArgumentParser()
p.add_argument("baseline", type=Path)
p.add_argument("candidate", type=Path)
p.add_argument("output", type=Path)
p.add_argument("--pairs", type=int, default=11)
p.add_argument("--iterations", type=int, default=1_000_000)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
metadata = {"pairs": a.pairs, "iterations": a.iterations, "views": 0}
try:
    allowed = sorted(os.sched_getaffinity(0))
    selected = allowed[0]
    os.sched_setaffinity(0, {selected})
    metadata.update(allowed_cpus=allowed, selected_cpu=selected,
                    effective_cpus=sorted(os.sched_getaffinity(0)))
except (AttributeError, OSError) as error:
    metadata["affinity_error"] = repr(error)
# Children inherit exactly the same affinity; timing instrumentation/flags are
# unchanged. Both case order and variant order alternate by pair.
bins = {"baseline": a.baseline.resolve(), "candidate": a.candidate.resolve()}
metadata["binaries"] = {k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()} for k, v in bins.items()}
(a.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
rows = []
with (a.output / "raw.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["variant", "pair", "case_order", "variant_order", "mode", "views", "iterations", "nanoseconds", "checksum"])
    for pair in range(a.pairs):
        modes = ["ordinary", "mixed"] if pair % 2 == 0 else ["mixed", "ordinary"]
        variants = ["baseline", "candidate"] if pair % 2 == 0 else ["candidate", "baseline"]
        for case_order, mode in enumerate(modes):
            checksums = []
            for variant_order, variant in enumerate(variants):
                result = subprocess.run([str(bins[variant]), mode, "0", str(a.iterations)], capture_output=True, text=True, check=True)
                fields = next(csv.reader([result.stdout.strip()]))
                assert fields[:3] == [mode, "0", str(a.iterations)]
                writer.writerow([variant, pair, case_order, variant_order] + fields)
                out.flush()
                rows.append((mode, pair, variant, int(fields[3])))
                checksums.append(fields[4])
            assert checksums[0] == checksums[1]
        print("pair", pair, "done", flush=True)
with (a.output / "paired.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["mode", "pair", "baseline_ns", "candidate_ns", "speedup", "candidate_faster"])
    summaries = []
    for mode in ["ordinary", "mixed"]:
        ratios, baseline, candidate = [], [], []
        for pair in range(a.pairs):
            values = {v: ns for m, n, v, ns in rows if m == mode and n == pair}
            b, c = values["baseline"], values["candidate"]
            baseline.append(b)
            candidate.append(c)
            ratios.append(b / c)
            writer.writerow([mode, pair, b, c, b / c, c < b])
        summaries.append([mode, statistics.median(baseline), statistics.median(candidate),
                          statistics.median(ratios), min(ratios), max(ratios),
                          sum(r > 1 for r in ratios), a.pairs,
                          min(baseline), max(baseline), min(candidate), max(candidate)])
with (a.output / "summary.csv").open("w", newline="") as out:
    writer = csv.writer(out)
    writer.writerow(["mode", "baseline_median_ns", "candidate_median_ns", "paired_speedup_median", "paired_speedup_min", "paired_speedup_max", "candidate_faster_pairs", "pairs", "baseline_min_ns", "baseline_max_ns", "candidate_min_ns", "candidate_max_ns"])
    writer.writerows(summaries)
