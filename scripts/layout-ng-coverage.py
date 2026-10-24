#!/usr/bin/env python3
"""Count how many WPT layout pages LayoutNG handles (W3COS_LAYOUT_NG_TRACE).

Runs each case's test document once through the runner's isolated worker
mode with W3COS_LAYOUT_NG=1 W3COS_LAYOUT_NG_TRACE=1 and classifies the page
by the per-compute trace lines: all-ng / mixed / fallback / no-layout.
Set W3COS_LAYOUT_NG_VERIFY=1 to also count legacy-vs-NG rect diffs.

Usage: layout-ng-coverage.py BIN WPT_ROOT REPO_ROOT OUT.tsv [STEP]

Per-case worker output (screenshots, reports) is only scratch: each case's
output dir is deleted as soon as the case finishes, and the run's temp dir is
removed on exit, including Ctrl-C/SIGTERM.
"""
import collections, concurrent.futures as cf, json, os, shutil, signal, subprocess, sys, tempfile
BIN, WPT, GATE, OUT = sys.argv[1:5]
STEP = int(sys.argv[5]) if len(sys.argv) > 5 else 1
PARTS = (("css2", ["--user-stylesheet", f"{GATE}/tests/wpt/profiles/css2-userstyle.css"]), ("css3", []))
env = dict(os.environ, W3COS_LAYOUT_NG_TRACE="1")
env.setdefault("W3COS_LAYOUT_NG", "1")
tmp = tempfile.mkdtemp(prefix="w3cos-ng-coverage-")


def _cleanup(*_):
    shutil.rmtree(tmp, ignore_errors=True)
    sys.exit(130)


signal.signal(signal.SIGTERM, _cleanup)

def run(part, extra, suite, i, t):
    mode = "testharness" if t["kind"] == "testharness" else "render"
    cmd = [BIN, "--wpt-root", WPT, "--suite", suite, "--timeout-ms", "10000", "--internal-mode", mode,
           "--internal-case-index", str(i), "--internal-output", f"{tmp}/{part}-{i}"] + extra
    try:
        p = subprocess.run(cmd, env=env, capture_output=True, timeout=40)
        err = p.stderr.decode("utf-8", "replace")
    except subprocess.TimeoutExpired:
        return t["path"], "timeout", "", 0
    finally:
        out = f"{tmp}/{part}-{i}"
        if os.path.isdir(out):
            shutil.rmtree(out, ignore_errors=True)
        elif os.path.exists(out):
            os.remove(out)
    ng = err.count("[layout-ng] ng")
    fb = err.count("[layout-ng] fallback")
    cls = "no-layout" if ng + fb == 0 else "all-ng" if fb == 0 else "fallback" if ng == 0 else "mixed"
    reasons = sorted({r for line in err.splitlines() if line.startswith("[layout-ng] fallback ")
                      for r in line.split()[2].split(",")})
    diffs = err.count("[layout-ng verify] #")
    return t["path"], cls, ",".join(reasons), diffs

jobs = []
try:
    with cf.ThreadPoolExecutor(8) as ex:
        for part, extra in PARTS:
            suite = f"{GATE}/tests/wpt/layout/suite-{part}.json"
            tests = json.load(open(suite))["tests"]
            for i in range(0, len(tests), STEP):
                jobs.append(ex.submit(run, part, extra, suite, i, tests[i]))
        res = [j.result() for j in jobs]
finally:
    shutil.rmtree(tmp, ignore_errors=True)
c = collections.Counter(r[1] for r in res)
with open(OUT, "w") as f:
    for p, cls, reasons, diffs in res:
        f.write(f"{cls}\t{p}\t{reasons}\t{diffs}\n")
print(len(res), dict(c))
sets = collections.Counter(r[2] for r in res if r[1] == "fallback")
print("top reason sets:")
for k, v in sets.most_common(15):
    print(f"  {v:5} {k}")
single = collections.Counter(r for x in res if x[1] == "fallback" for r in x[2].split(","))
print("pages blocked by reason:", dict(single.most_common()))
if any(r[3] for r in res):
    print("pages with verify diffs:", sum(1 for r in res if r[3]))
