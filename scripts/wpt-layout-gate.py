#!/usr/bin/env python3
"""Layout WPT gate for the native engine (LayoutNG rewrite merge gate).

Suites:   tests/wpt/layout/suite-css2.json (run with the CSS2 user-stylesheet
          profile) and suite-css3.json, pinned to the WPT revision recorded in
          each file.
Baseline: tests/wpt/layout-baseline-pass.txt -- one "<path>\t<kind>" line per
          test that passed on the baseline engine.  The gate passes when the
          new pass set is a superset of the baseline (zero regressions).

Usage:
  wpt-layout-gate.py run   --bin target/release/w3cos-wpt --wpt-root ../wpt --out DIR [--jobs 8] [--chunk 500]
      Resumable: each part runs in --case-start/--case-limit chunks written to
      DIR/<part>/chunk-<start>/results.json; finished chunks are skipped on
      rerun, so an interrupted run loses at most one chunk.
  wpt-layout-gate.py check --results DIR [--baseline FILE] [--write-baseline]
  wpt-layout-gate.py table --results DIR [--results DIR2 ...]
"""
import argparse, collections, json, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LAYOUT = os.path.join(ROOT, "tests", "wpt", "layout")
PARTS = (("css2", ["--user-stylesheet", os.path.join(ROOT, "tests/wpt/profiles/css2-userstyle.css")]),
         ("css3", []))


def dir_of(path):
    s = path.split("/")
    return "/".join(s[:3] if path.startswith("css/CSS2/") else s[:2])


def report_files(results_dir, part):
    whole = os.path.join(results_dir, part, "results.json")
    if os.path.exists(whole):
        return [whole]
    base = os.path.join(results_dir, part)
    return sorted(os.path.join(base, d, "results.json") for d in os.listdir(base)
                  if d.startswith("chunk-") and os.path.exists(os.path.join(base, d, "results.json")))


def load(results_dir):
    out = {}
    for part, _ in PARTS:
        for path in report_files(results_dir, part):
            with open(path) as f:
                for t in json.load(f)["tests"]:
                    out[(t["path"], t["kind"])] = t["status"]
    return out


def suite_keys():
    keys = set()
    for part, _ in PARTS:
        with open(os.path.join(LAYOUT, f"suite-{part}.json")) as f:
            keys |= {(t["path"], t["kind"]) for t in json.load(f)["tests"]}
    return keys


def read_baseline(path):
    with open(path) as f:
        return {tuple(l.rstrip("\n").split("\t")) for l in f if l.strip() and not l.startswith("#")}


def cmd_run(a):
    for part, extra in PARTS:
        suite = os.path.join(LAYOUT, f"suite-{part}.json")
        with open(suite) as f:
            n = len(json.load(f)["tests"])
        for start in range(0, n, a.chunk):
            out = os.path.join(a.out, part, f"chunk-{start:05d}")
            if os.path.exists(os.path.join(out, "results.json")):
                continue
            cmd = [a.bin, "--wpt-root", a.wpt_root, "--suite", suite, "--artifacts", out,
                   "--case-start", str(start), "--case-limit", str(a.chunk), "--jobs", str(a.jobs),
                   "--failure-artifacts-only", "--report-only"] + extra
            print(f"+ {part} {start}/{n}", " ".join(cmd), flush=True)
            subprocess.run(cmd, cwd=ROOT, check=True)


def cmd_table(a):
    rs = [load(d) for d in a.results]
    universe = suite_keys()
    tot = collections.Counter(dir_of(p) for p, _ in universe)
    print("| dir | total | " + " | ".join(os.path.basename(d.rstrip("/")) for d in a.results) + " |")
    print("|---|---:|" + "---:|" * len(rs))
    sums = [0] * len(rs)
    for d in sorted(tot):
        row = []
        for i, r in enumerate(rs):
            n = sum(1 for (p, _), st in r.items() if st == "pass" and dir_of(p) == d)
            sums[i] += n
            row.append(str(n))
        print(f"| {d} | {tot[d]} | " + " | ".join(row) + " |")
    print(f"| **all** | {sum(tot.values())} | " + " | ".join(str(s) for s in sums) + " |")


def cmd_check(a):
    r = load(a.results)
    passed = {k for k, st in r.items() if st == "pass"}
    if a.write_baseline:
        with open(a.baseline, "w") as f:
            f.write("# path\tkind -- layout WPT tests passing on the baseline engine\n")
            for p, k in sorted(passed):
                f.write(f"{p}\t{k}\n")
        print(f"wrote {len(passed)} baseline passes to {a.baseline}")
        return 0
    base = read_baseline(a.baseline)
    regressed = sorted(base - passed)
    gained = sorted(passed - base)
    for p, k in regressed:
        print(f"REGRESSION {p} ({k}): {r.get((p, k), 'missing')}")
    print(f"baseline={len(base)} now={len(passed)} regressions={len(regressed)} new_passes={len(gained)}")
    return 1 if regressed else 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("run"); p.add_argument("--bin", required=True); p.add_argument("--wpt-root", required=True)
    p.add_argument("--out", required=True); p.add_argument("--jobs", type=int, default=8)
    p.add_argument("--chunk", type=int, default=500)
    p = sub.add_parser("check"); p.add_argument("--results", required=True)
    p.add_argument("--baseline", default=os.path.join(ROOT, "tests", "wpt", "layout-baseline-pass.txt"))
    p.add_argument("--write-baseline", action="store_true")
    p = sub.add_parser("table"); p.add_argument("--results", action="append", required=True)
    a = ap.parse_args()
    return {"run": cmd_run, "check": cmd_check, "table": cmd_table}[a.cmd](a) or 0


if __name__ == "__main__":
    sys.exit(main())
