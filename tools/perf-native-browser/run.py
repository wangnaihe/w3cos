#!/usr/bin/env python3
"""Compare w3cos native (AOT + CPU/headless raster) with headless Chromium.

The page source is tools/perf-native-browser/workloads.ts. Chromium executes
those bytes as an ES module. w3cos compiles the same bytes and runs the
generated native binary. Steady-state timings come from performance.now()
inside the page. Cold start is a separate workload and includes process
launch on both sides.

No browser security feature is disabled to move the result. --disable-gpu is
set because this host has no DRM/GPU node; the native side uses tiny-skia for
the same reason.
"""
import argparse
import hashlib
import http.server
import json
import math
import os
import platform
import shutil
import statistics
import subprocess
import threading
import time
from pathlib import Path

VIEWPORT = {"width": 1024, "height": 768, "dpr": 1}
WORKLOADS = (
    "startup",
    "dom-list",
    "layout",
    "layout-css",
    "frames",
    "compute",
    "memory",
    "css-list",
    "flex-list",
    "events",
)
# Per-side limits in milliseconds. Every WORKLOADS entry must appear here;
# a missing key aborts the run before results are written.
TIMEOUTS_MS = {
    "startup": 60000,
    "dom-list": 180000,
    "layout": 180000,
    "layout-css": 180000,
    "frames": 240000,
    "compute": 120000,
    "memory": 180000,
    "css-list": 180000,
    "flex-list": 240000,
    "events": 180000,
}
# Production `w3cos build --release` embeds the size profile. speed and max
# are measurement-only. panic stays unwind in every profile: JS exceptions
# use catch_unwind, so panic=abort is not a compatible application setting.
# PGO for max is rustc profile-generate/profile-use, trained on a different
# trip count and stride than the measured run.
PROFILE_SETTINGS = {
    "size": {
        "release_profile": "size",
        "opt_level": "z",
        "lto": "fat",
        "codegen_units": "1",
        "pgo": False,
        "target_cpu": None,
        "noted_only": False,
    },
    "speed": {
        "release_profile": "speed",
        "opt_level": "3",
        "lto": "off",
        "codegen_units": "16",
        "pgo": False,
        "target_cpu": None,
        "noted_only": False,
    },
    "max": {
        "release_profile": "max",
        "opt_level": "3",
        "lto": "fat",
        "codegen_units": "1",
        "pgo": True,
        "target_cpu": None,
        "noted_only": False,
    },
    "speed-native": {
        "release_profile": "speed",
        "opt_level": "3",
        "lto": "off",
        "codegen_units": "16",
        "pgo": False,
        "target_cpu": "native",
        "noted_only": True,
    },
}
PRIMARY_PROFILES = ("size", "speed", "max")
PGO_TRAIN_SCALE = "0.5"
PGO_TRAIN_SEED = "13"
# Literal substitutions for the instrumented binary only. The measured source
# keeps the published constants so AOT fusion still sees them.
PGO_TRAIN_SUBSTITUTIONS = (
    ("var DOM_NODES = 1000;", "var DOM_NODES = 500;"),
    ("var DOM_UPDATES = 8000;", "var DOM_UPDATES = 4000;"),
    ("var FLEX_ITEMS = 240;", "var FLEX_ITEMS = 120;"),
    ("var GRID_ITEMS = 240;", "var GRID_ITEMS = 120;"),
    ("var SCROLL_ROWS = 100;", "var SCROLL_ROWS = 50;"),
    ("var BOXES = 80;", "var BOXES = 40;"),
    ("var COMPUTE_N = 400000;", "var COMPUTE_N = 200000;"),
    ("var MEMORY_NODES = 4000;", "var MEMORY_NODES = 2000;"),
    ("var CSS_LIST_NODES = 1000;", "var CSS_LIST_NODES = 500;"),
    ("var FLEX_LIST_NODES = 400;", "var FLEX_LIST_NODES = 200;"),
    ("var EVENT_NODES = 400;", "var EVENT_NODES = 200;"),
    ("var cards = 40;", "var cards = 20;"),
    ("(k * 17)", "(k * 13)"),
    ("(frameIndex * 17)", "(frameIndex * 13)"),
)
CHROME = os.environ.get("W3COS_CHROME", "/usr/bin/google-chrome")
CHROME_ARGS = [
    "--disable-gpu",
    "--force-device-scale-factor=1",
    "--window-size=1024,768",
    "--disable-dev-shm-usage",
    "--no-first-run",
    "--no-default-browser-check",
]


def median(values):
    if not values:
        return None
    return statistics.median(values)


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * fraction
    low = math.floor(rank)
    high = math.ceil(rank)
    if low == high:
        return ordered[low]
    weight = rank - low
    return ordered[low] * (1.0 - weight) + ordered[high] * weight


def spread(values):
    if not values:
        return None
    return {"min": min(values), "median": median(values), "max": max(values), "n": len(values)}


def read_text(path):
    try:
        return Path(path).read_text()
    except OSError:
        return ""


def process_rows():
    rows = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        stat = read_text(f"/proc/{pid}/stat")
        status = read_text(f"/proc/{pid}/status")
        if not stat or not status:
            continue
        end = stat.rfind(")")
        if end < 0:
            continue
        fields = stat[end + 1:].split()
        if len(fields) < 13:
            continue
        rss = 0
        for line in status.splitlines():
            if line.startswith("VmRSS:"):
                rss = int(line.split()[1])
                break
        rows[pid] = {
            "ppid": int(fields[1]),
            "cpu": int(fields[11]) + int(fields[12]),
            "rss": rss,
        }
    return rows


def tree_ids(roots, rows):
    selected = set()
    stack = list(roots)
    while stack:
        pid = stack.pop()
        if pid in selected or pid not in rows:
            continue
        selected.add(pid)
        for other, row in rows.items():
            if row["ppid"] == pid:
                stack.append(other)
    return selected


def tree_rss_cpu(roots):
    rows = process_rows()
    ids = tree_ids(roots, rows)
    rss = sum(rows[pid]["rss"] for pid in ids)
    cpu = sum(rows[pid]["cpu"] for pid in ids)
    return rss, cpu, ids


def chrome_roots():
    roots = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        cmd = read_text(f"/proc/{name}/cmdline").replace("\0", " ")
        if "google-chrome" in cmd or "/chrome" in cmd:
            roots.append(int(name))
    return roots


def clk_tck():
    return os.sysconf("SC_CLK_TCK")


def cpu_ms(ticks):
    return ticks * 1000.0 / clk_tck()


def host_info(repo):
    model = ""
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.lower().startswith("model name"):
                model = line.split(":", 1)[1].strip()
                break
    except OSError:
        model = platform.processor()
    chrome = subprocess.check_output([CHROME, "--version"], text=True).strip()
    rustc = subprocess.check_output(["rustc", "-Vv"], text=True).strip()
    revision = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
    return {
        "os": " ".join(platform.uname()),
        "cpu_model": model,
        "cpus": os.cpu_count(),
        "gpu": Path("/dev/dri").exists(),
        "native_raster": "tiny-skia CPU (no /dev/dri, skia/gpu features off)",
        "chrome": chrome,
        "rustc": rustc,
        "git": revision,
        "viewport": VIEWPORT,
        "chrome_args": CHROME_ARGS,
        "panic": "unwind",
        "panic_abort": "incompatible: JavaScript exceptions use catch_unwind",
        "pgo_training": {
            "scale": PGO_TRAIN_SCALE,
            "seed": PGO_TRAIN_SEED,
            "measured_scale": 1,
            "measured_seed": 17,
            "note": (
                "The instrumented binary is a separate compile of workloads.ts with "
                "half the node counts and stride 13. The measured max binary is the "
                "published source, rebuilt with that profile. Runtime crates match "
                "across the two compiles. Application counters that do not match the "
                "published control flow are ignored, so the measured trip counts are "
                "not the training set."
            ),
        },
    }


class WorkloadServer(threading.Thread):
    def __init__(self, source):
        super().__init__(daemon=True)
        self.source = source
        handler = self._handler()
        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.port = self.httpd.server_address[1]

    def _handler(self):
        source = self.source
        noto = Path("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf")
        noto_bytes = noto.read_bytes() if noto.is_file() else b""

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, fmt, *args):
                return

            def do_GET(self):
                path = self.path.split("?", 1)[0]
                if path == "/workloads.ts":
                    body = source.encode()
                    content_type = "text/javascript; charset=utf-8"
                elif path == "/NotoSans-Regular.ttf":
                    body = noto_bytes
                    content_type = "font/ttf"
                else:
                    workload = "startup"
                    if "workload=" in self.path:
                        workload = self.path.split("workload=", 1)[1].split("&", 1)[0]
                    body = (
                        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>"
                        f"<script>globalThis.W3COS_PERF_WORKLOAD = {json.dumps(workload)};</script>"
                        "<script type=\"module\" src=\"/workloads.ts\"></script>"
                        "</body></html>"
                    ).encode()
                    content_type = "text/html; charset=utf-8"
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Cache-Control", "no-store")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        return Handler

    def run(self):
        self.httpd.serve_forever()

    def close(self):
        self.httpd.shutdown()


def build_cli(repo, jobs):
    env = os.environ.copy()
    env["CARGO_BUILD_JOBS"] = str(jobs)
    subprocess.check_call(["cargo", "build", "--release", "-p", "w3cos-cli"], cwd=repo, env=env)
    return repo / "target" / "release" / "w3cos"


def llvm_profdata():
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    host = subprocess.check_output(["rustc", "-vV"], text=True)
    target = ""
    for line in host.splitlines():
        if line.startswith("host:"):
            target = line.split(":", 1)[1].strip()
    path = Path(sysroot) / "lib" / "rustlib" / target / "bin" / "llvm-profdata"
    if not path.is_file():
        raise SystemExit(
            f"llvm-profdata is not installed at {path}. "
            "Install it with: rustup component add llvm-tools"
        )
    return path


def apply_profile_env(env, settings, rustflags=None):
    env["W3COS_RUNTIME_FEATURES"] = "cpu-render"
    env["W3COS_RELEASE_PROFILE"] = settings["release_profile"]
    env["CARGO_PROFILE_RELEASE_OPT_LEVEL"] = settings["opt_level"]
    env["CARGO_PROFILE_RELEASE_LTO"] = settings["lto"]
    env["CARGO_PROFILE_RELEASE_CODEGEN_UNITS"] = settings["codegen_units"]
    env["CARGO_PROFILE_RELEASE_PANIC"] = "unwind"
    env["CARGO_INCREMENTAL"] = "0"
    env.pop("W3COS_NO_STRIP", None)
    env.pop("W3COS_AOT_WINDOW", None)
    flags = []
    if settings.get("target_cpu"):
        flags.append(f"-Ctarget-cpu={settings['target_cpu']}")
    if rustflags:
        flags.append(rustflags)
    if flags:
        env["RUSTFLAGS"] = " ".join(flags)
    else:
        env.pop("RUSTFLAGS", None)
    return env


def compile_native(repo, cli, jobs, build_root, settings, binary, target_dir, fixture, rustflags=None):
    build_root.mkdir(parents=True, exist_ok=True)
    target_dir.mkdir(parents=True, exist_ok=True)
    env = apply_profile_env(os.environ.copy(), settings, rustflags)
    env["CARGO_BUILD_JOBS"] = str(jobs)
    env["TMPDIR"] = str(build_root)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    subprocess.check_call(
        [str(cli), "build", str(fixture), "--release", "--output", str(binary)],
        cwd=repo,
        env=env,
    )
    return binary


def binary_identity(path):
    data = Path(path).read_bytes()
    return {
        "path": str(path),
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def training_fixture(repo, dest):
    text = (repo / "tools" / "perf-native-browser" / "workloads.ts").read_text()
    for old, new in PGO_TRAIN_SUBSTITUTIONS:
        count = text.count(old)
        if count != 1:
            raise SystemExit(f"training substitution {old!r} matched {count} times")
        text = text.replace(old, new, 1)
    dest.write_text(text)
    return dest


def train_pgo(binary, workloads, prof_dir, timeouts):
    prof_dir.mkdir(parents=True, exist_ok=True)
    for stale in prof_dir.glob("*.profraw"):
        stale.unlink()
    extra = {"LLVM_PROFILE_FILE": str(prof_dir / "train-%p.profraw")}
    for workload in workloads:
        print(
            f"== pgo train {workload} scale={PGO_TRAIN_SCALE} seed={PGO_TRAIN_SEED}",
            flush=True,
        )
        result = run_native(binary, workload, timeouts[workload] / 1000.0, extra_env=extra)
        if result.get("status") != "ok":
            raise SystemExit(f"PGO training failed for {workload}: {result.get('tail') or result}")
    raw = list(prof_dir.glob("*.profraw"))
    if not raw:
        raise SystemExit(f"PGO training produced no .profraw files in {prof_dir}")
    merged = prof_dir / "merged.profdata"
    subprocess.check_call([str(llvm_profdata()), "merge", "-o", str(merged), *map(str, raw)])
    return merged


def build_profile(repo, cli, jobs, build_root, name, workloads, timeouts):
    settings = PROFILE_SETTINGS[name]
    fixture = repo / "tools" / "perf-native-browser" / "workloads.ts"
    final_binary = build_root / f"w3cos-bench-{name}"
    final_target = repo / "target" / f"perf-{name}"
    pgo = None
    if settings["pgo"]:
        instr_binary = build_root / f"w3cos-bench-{name}-instrumented"
        instr_target = repo / "target" / f"perf-{name}-instrumented"
        prof_dir = build_root / f"pgo-{name}"
        train_source = training_fixture(repo, build_root / "workloads-pgo-train.ts")
        print(f"== build {name} instrumented", flush=True)
        compile_native(
            repo,
            cli,
            jobs,
            build_root,
            settings,
            instr_binary,
            instr_target,
            train_source,
            rustflags=f"-Cprofile-generate={prof_dir}",
        )
        merged = train_pgo(instr_binary, workloads, prof_dir, timeouts)
        print(f"== build {name} profile-use", flush=True)
        compile_native(
            repo,
            cli,
            jobs,
            build_root,
            settings,
            final_binary,
            final_target,
            fixture,
            rustflags=f"-Cprofile-use={merged}",
        )
        pgo = {
            "merged_profdata": str(merged),
            "profdata_bytes": merged.stat().st_size,
            "training_scale": PGO_TRAIN_SCALE,
            "training_seed": PGO_TRAIN_SEED,
            "instrumented": binary_identity(instr_binary),
        }
    else:
        print(f"== build {name}", flush=True)
        compile_native(repo, cli, jobs, build_root, settings, final_binary, final_target, fixture)
    identity = binary_identity(final_binary)
    identity.update({
        "profile": name,
        "settings": settings,
        "pgo": pgo,
        "stripped": True,
    })
    return final_binary, identity


def parse_result_line(text):
    marker = "W3COS_PERF_RESULT "
    index = text.find(marker)
    if index < 0:
        return None
    try:
        return json.loads(text[index + len(marker):].strip())
    except json.JSONDecodeError:
        return None


def run_native(binary, workload, timeout, extra_env=None):
    env = os.environ.copy()
    env["W3COS_JS_CONSOLE"] = "1"
    env["W3COS_HEADLESS_PUMP"] = "1"
    env["W3COS_PERF_WORKLOAD"] = workload
    env["W3COS_VIEWPORT_W"] = str(VIEWPORT["width"])
    env["W3COS_VIEWPORT_H"] = str(VIEWPORT["height"])
    env.pop("W3COS_AOT_WINDOW", None)
    env.pop("W3COS_PERF_SCALE", None)
    env.pop("W3COS_PERF_SEED", None)
    if extra_env:
        env.update(extra_env)
    started = time.perf_counter()
    proc = subprocess.Popen(
        [str(binary)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
        text=True,
        bufsize=1,
    )
    lines = []
    first_frame = None
    peak = 0

    def read_output():
        nonlocal first_frame
        for line in proc.stdout:
            stamp = time.perf_counter() - started
            lines.append(line.rstrip("\n"))
            if first_frame is None and line.startswith("W3COS_FRAME "):
                first_frame = stamp

    def sample():
        nonlocal peak
        while proc.poll() is None:
            rss, _, _ = tree_rss_cpu([proc.pid])
            peak = max(peak, rss)
            time.sleep(0.02)

    reader = threading.Thread(target=read_output)
    sampler = threading.Thread(target=sample)
    reader.start()
    sampler.start()
    reader.join(timeout)
    if reader.is_alive():
        proc.kill()
        reader.join(5)
        sampler.join(5)
        return {"status": "timeout", "lines": lines, "peak_rss_kb": peak}
    proc.wait(timeout=10)
    sampler.join(2)
    rss, _, _ = tree_rss_cpu([proc.pid]) if proc.poll() is None else (0, 0, set())
    text = "\n".join(lines)
    result = None
    for line in lines:
        parsed = parse_result_line(line)
        if parsed is not None:
            result = parsed
    frames = [line for line in lines if line.startswith("W3COS_FRAME ")]
    procs = [line for line in lines if line.startswith("W3COS_PROC ")]
    content = next((line for line in lines if "W3COS_FIRST_CONTENT" in line), "")
    return {
        "status": "ok" if proc.returncode == 0 and "W3COS_HEADLESS_ERROR" not in text else "error",
        "code": proc.returncode,
        "cold_ms": None if first_frame is None else first_frame * 1000.0,
        "wall_ms": (time.perf_counter() - started) * 1000.0,
        "peak_rss_kb": max(peak, rss),
        "steady_rss_kb": steady_from_proc(procs),
        "cpu_ms": cpu_from_proc(procs, "done" if workload in ("startup", "frames") else "script"),
        "result": result,
        "frames": frames,
        "content": content,
        "procs": procs,
        "tail": lines[-8:],
    }


def steady_from_proc(lines):
    rss = None
    for line in lines:
        if "phase=done" in line or "phase=script" in line:
            for part in line.split():
                if part.startswith("rss_kb="):
                    rss = int(float(part.split("=", 1)[1]))
    return rss


def cpu_from_proc(lines, phase):
    for line in reversed(lines):
        if f"phase={phase}" in line:
            for part in line.split():
                if part.startswith("cpu_ms="):
                    return float(part.split("=", 1)[1])
    return None


def frame_totals(lines):
    totals = []
    paints = []
    for line in lines:
        fields = dict(part.split("=", 1) for part in line.split() if "=" in part)
        if "total_ms" in fields:
            totals.append(float(fields["total_ms"]))
        if "paint_ms" in fields:
            paints.append(float(fields["paint_ms"]))
    return totals, paints


def run_browser(playwright, server, workload, timeout_ms):
    started = time.perf_counter()
    browser = playwright.chromium.launch(
        executable_path=CHROME,
        headless=True,
        args=CHROME_ARGS,
    )
    try:
        context = browser.new_context(viewport={"width": VIEWPORT["width"], "height": VIEWPORT["height"]}, device_scale_factor=1)
        page = context.new_page()
        roots = chrome_roots()
        before_rss, before_cpu, _ = tree_rss_cpu(roots)
        peak = before_rss
        stop = False

        def sample():
            nonlocal peak
            while not stop:
                rss, _, _ = tree_rss_cpu(chrome_roots())
                peak = max(peak, rss)
                time.sleep(0.02)

        sampler = threading.Thread(target=sample)
        sampler.start()
        token = "W3COS_FIRST_CONTENT" if workload == "startup" else "W3COS_PERF_RESULT"
        url = f"http://127.0.0.1:{server.port}/?workload={workload}"
        with page.expect_console_message(lambda message: token in message.text, timeout=timeout_ms) as info:
            page.goto(url, wait_until="commit")
        message = info.value.text
        cold_ms = (time.perf_counter() - started) * 1000.0
        _, after_cpu, _ = tree_rss_cpu(chrome_roots())
        time.sleep(0.25)
        steady, _, _ = tree_rss_cpu(chrome_roots())
        stop = True
        sampler.join(2)
        return {
            "status": "ok",
            "cold_ms": cold_ms,
            "peak_rss_kb": peak,
            "steady_rss_kb": steady,
            "cpu_ms": cpu_ms(max(0, after_cpu - before_cpu)),
            "result": parse_result_line(message),
            "content": message if "W3COS_FIRST_CONTENT" in message else "",
            "message": message,
        }
    finally:
        browser.close()


def sample_values(result, key="elapsed_ms"):
    if not result or result.get("status") not in (None, "ok"):
        if result and result.get("status") == "error":
            return []
    payload = result or {}
    if payload.get("status") == "error":
        return []
    samples = payload.get("samples")
    if samples is None:
        if key in payload:
            return [payload[key]]
        return []
    values = []
    for sample in samples:
        if isinstance(sample, (int, float)):
            values.append(float(sample))
        elif isinstance(sample, dict) and key in sample:
            values.append(float(sample[key]))
    return values


def run_median(result, key="elapsed_ms"):
    return median(sample_values(result, key))


def checksum_of(result):
    if not result:
        return None
    samples = result.get("samples")
    if isinstance(samples, list) and samples and isinstance(samples[0], dict) and "checksum" in samples[0]:
        return samples[0]["checksum"]
    return result.get("checksum")


def numbers_close(left, right, rel=0.02, abs_tol=1.0):
    if left is None or right is None:
        return False
    if isinstance(left, str) or isinstance(right, str):
        return left == right
    return abs(left - right) <= max(abs_tol, rel * max(abs(left), abs(right)))


def comparable(workload, native, browser):
    native_result = native.get("result")
    browser_result = browser.get("result")
    if workload == "startup":
        return "w3cos-perf-ready" in native.get("content", "") and "w3cos-perf-ready" in browser.get("content", "")
    if not native_result or not browser_result:
        return False
    if native_result.get("status") != "ok" or browser_result.get("status") != "ok":
        return False
    if workload == "dom-list":
        native_sample = native_result["samples"][0]
        browser_sample = browser_result["samples"][0]
        return (
            native_sample["count"] == browser_sample["count"]
            and native_sample["checksum"] == browser_sample["checksum"]
            and native_sample["first"] == browser_sample["first"]
            and native_sample["last"] == browser_sample["last"]
        )
    if workload == "compute":
        return numbers_close(checksum_of(native_result), checksum_of(browser_result), rel=1e-3, abs_tol=1e-3)
    if workload == "frames":
        return (
            native_result.get("frames") == browser_result.get("frames")
            and numbers_close(native_result.get("checksum"), browser_result.get("checksum"), rel=0.02, abs_tol=4)
        )
    if workload in ("layout", "layout-css", "memory", "css-list", "flex-list"):
        count_ok = True
        if workload in ("memory", "css-list", "flex-list"):
            count_ok = native_result.get("count") == browser_result.get("count")
        return count_ok and numbers_close(checksum_of(native_result), checksum_of(browser_result), rel=0.02, abs_tol=8)
    if workload == "events":
        return (
            native_result.get("count") == browser_result.get("count")
            and checksum_of(native_result) == checksum_of(browser_result)
        )
    return False


def summarize_runs(workload, native_runs, browser_runs):
    ok_native = [run for run in native_runs if run.get("status") == "ok"]
    ok_browser = [run for run in browser_runs if run.get("status") == "ok"]
    paired = min(len(ok_native), len(ok_browser))
    agreement = [comparable(workload, ok_native[i], ok_browser[i]) for i in range(paired)]
    rows = []

    def add(metric, native_values, browser_values, lower_is_better=True):
        native_stat = spread(native_values)
        browser_stat = spread(browser_values)
        ratio = None
        if native_stat and browser_stat and browser_stat["median"] not in (0, None):
            ratio = native_stat["median"] / browser_stat["median"]
        rows.append({
            "workload": workload,
            "metric": metric,
            "w3cos": native_stat,
            "chromium": browser_stat,
            "ratio_w3cos_over_chromium": ratio,
            "lower_is_better": lower_is_better,
            "comparable": all(agreement) if agreement else False,
        })

    if workload == "startup":
        add("cold_start_ms", [run["cold_ms"] for run in ok_native if run.get("cold_ms") is not None],
            [run["cold_ms"] for run in ok_browser if run.get("cold_ms") is not None])
        native_paint = []
        for run in ok_native:
            _totals, paints = frame_totals(run.get("frames") or [])
            if paints:
                native_paint.append(paints[0])
        if native_paint:
            rows.append({
                "workload": workload,
                "metric": "native_first_raster_ms",
                "w3cos": spread(native_paint),
                "chromium": None,
                "ratio_w3cos_over_chromium": None,
                "lower_is_better": True,
                "comparable": False,
                "note": "Included in native cold start. Chromium's cold start stops at the rAF signal and does not PNG-encode the frame.",
            })
        add("cpu_ms", [run["cpu_ms"] for run in ok_native if run.get("cpu_ms") is not None],
            [run["cpu_ms"] for run in ok_browser if run.get("cpu_ms") is not None])
        add("peak_rss_kb", [run["peak_rss_kb"] for run in ok_native if run.get("peak_rss_kb")],
            [run["peak_rss_kb"] for run in ok_browser if run.get("peak_rss_kb")])
        add("steady_rss_kb", [run["steady_rss_kb"] for run in ok_native if run.get("steady_rss_kb")],
            [run["steady_rss_kb"] for run in ok_browser if run.get("steady_rss_kb")])
    else:
        if workload != "frames":
            add("wall_ms", [run_median(run.get("result")) for run in ok_native if run_median(run.get("result")) is not None],
                [run_median(run.get("result")) for run in ok_browser if run_median(run.get("result")) is not None])
        if workload == "dom-list":
            add("create_ms", [run_median(run.get("result"), "create_ms") for run in ok_native if run_median(run.get("result"), "create_ms") is not None],
                [run_median(run.get("result"), "create_ms") for run in ok_browser if run_median(run.get("result"), "create_ms") is not None])
            add("update_ms", [run_median(run.get("result"), "update_ms") for run in ok_native if run_median(run.get("result"), "update_ms") is not None],
                [run_median(run.get("result"), "update_ms") for run in ok_browser if run_median(run.get("result"), "update_ms") is not None])
        if workload == "frames":
            def run_percentile(run, fraction, key="elapsed_ms"):
                values = sample_values(run.get("result"), key)
                if key == "elapsed_ms" and not values:
                    values = sample_values(run.get("result"))
                return percentile(values, fraction) if values else None

            def gap_percentile(run, fraction):
                result = run.get("result") or {}
                gaps = [gap for gap in result.get("gaps") or [] if gap]
                return percentile(gaps, fraction) if gaps else None

            native_p50 = [value for run in ok_native if (value := run_percentile(run, 0.50)) is not None]
            browser_p50 = [value for run in ok_browser if (value := run_percentile(run, 0.50)) is not None]
            native_p95 = [value for run in ok_native if (value := run_percentile(run, 0.95)) is not None]
            browser_p95 = [value for run in ok_browser if (value := run_percentile(run, 0.95)) is not None]
            native_pipeline = []
            for run in ok_native:
                totals, _ = frame_totals(run.get("frames") or [])
                measured = totals[2:]
                if measured:
                    native_pipeline.append(percentile(measured, 0.50))
            browser_gaps = [value for run in ok_browser if (value := gap_percentile(run, 0.50)) is not None]
            add("frame_work_p50_ms", native_p50, browser_p50)
            add("frame_work_p95_ms", native_p95, browser_p95)
            add("native_pipeline_p50_ms", native_pipeline, [])
            rows[-1]["note"] = "Pipeline time is rAF callback plus tiny-skia raster. Browser gap is reported separately."
            add("browser_raf_gap_p50_ms", [], browser_gaps)
        add("cpu_ms", [run["cpu_ms"] for run in ok_native if run.get("cpu_ms") is not None],
            [run["cpu_ms"] for run in ok_browser if run.get("cpu_ms") is not None])
        add("peak_rss_kb", [run["peak_rss_kb"] for run in ok_native if run.get("peak_rss_kb")],
            [run["peak_rss_kb"] for run in ok_browser if run.get("peak_rss_kb")])
        add("steady_rss_kb", [run["steady_rss_kb"] for run in ok_native if run.get("steady_rss_kb")],
            [run["steady_rss_kb"] for run in ok_browser if run.get("steady_rss_kb")])
        add(
            "checksum",
            [value for run in ok_native if (value := numeric_checksum(run)) is not None],
            [value for run in ok_browser if (value := numeric_checksum(run)) is not None],
            lower_is_better=False,
        )
    return {"agreement": agreement, "rows": rows, "native_runs": native_runs, "browser_runs": browser_runs}


def numeric_checksum(run):
    value = checksum_of(run.get("result"))
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)


def format_stat(stat, metric=""):
    if not stat or stat.get("median") is None:
        return "n/a"
    if "rss" in metric:
        def mb(value):
            return value / 1024.0
        return f"{mb(stat['median']):.1f} MB ({mb(stat['min']):.1f}–{mb(stat['max']):.1f}, n={stat['n']})"
    if metric == "checksum":
        return f"{stat['median']:.6g} ({stat['min']:.6g}–{stat['max']:.6g}, n={stat['n']})"
    return f"{stat['median']:.3f} ({stat['min']:.3f}–{stat['max']:.3f}, n={stat['n']})"


def format_table(summaries):
    lines = [
        "| workload | metric | w3cos median (min–max) | Chromium median (min–max) | w3cos/Chromium | comparable |",
        "| --- | --- | ---: | ---: | ---: | --- |",
    ]
    for summary in summaries:
        for row in summary["rows"]:
            ratio = row["ratio_w3cos_over_chromium"]
            ratio_text = "n/a" if ratio is None else f"{ratio:.3f}"
            lines.append(
                f"| {row['workload']} | {row['metric']} | {format_stat(row['w3cos'], row['metric'])} | {format_stat(row['chromium'], row['metric'])} | {ratio_text} | {row['comparable']} |"
            )
    return "\n".join(lines)


def format_profile_comparison(order, by_profile):
    """One Chromium column beside every native profile. Noted profiles stay in the table."""
    if not order:
        return ""
    first = by_profile[order[0]]
    header = ["workload", "metric", *[name for name in order], "Chromium", "comparable"]
    lines = [
        "| " + " | ".join(header) + " |",
        "| " + " | ".join(["---", "---", *["---:" for _ in order], "---:", "---"]) + " |",
    ]
    for index, summary in enumerate(first):
        metrics = [row["metric"] for row in summary["rows"]]
        for metric in metrics:
            workload = summary["rows"][0]["workload"]
            cells = [workload, metric]
            chromium = "n/a"
            comparable = True
            for name in order:
                row = next(item for item in by_profile[name][index]["rows"] if item["metric"] == metric)
                cells.append(format_stat(row["w3cos"], metric))
                chromium = format_stat(row["chromium"], metric)
                comparable = comparable and row["comparable"]
            cells.extend([chromium, str(comparable)])
            lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines)


def self_check():
    assert median([1, 3, 2]) == 2
    assert median([1, 2, 3, 4]) == 2.5
    assert percentile([1, 2, 3, 4], 0) == 1
    assert percentile([1, 2, 3, 4], 1) == 4
    assert abs(percentile([1, 2, 3, 4], 0.5) - 2.5) < 1e-9
    assert numbers_close(1.0, 1.01, rel=0.02, abs_tol=0)
    assert not numbers_close(1.0, 2.0, rel=0.02, abs_tol=0)
    assert set(TIMEOUTS_MS) == set(WORKLOADS)
    assert set(PRIMARY_PROFILES) <= set(PROFILE_SETTINGS)
    assert PROFILE_SETTINGS["size"]["opt_level"] == "z"
    assert PROFILE_SETTINGS["size"]["lto"] == "fat"
    assert PROFILE_SETTINGS["speed"]["lto"] == "off"
    assert PROFILE_SETTINGS["speed"]["codegen_units"] == "16"
    assert PROFILE_SETTINGS["max"]["lto"] == "fat"
    assert PROFILE_SETTINGS["max"]["pgo"] is True
    assert PROFILE_SETTINGS["speed-native"]["noted_only"] is True
    assert PROFILE_SETTINGS["speed-native"]["target_cpu"] == "native"
    assert all(old.count("\n") == 0 for old, _new in PGO_TRAIN_SUBSTITUTIONS)
    sample = "var DOM_NODES = 1000;\nvar cards = 40;\n(k * 17)\n(frameIndex * 17)\n"
    for old, _new in (
        ("var DOM_NODES = 1000;", "var DOM_NODES = 500;"),
        ("var cards = 40;", "var cards = 20;"),
        ("(k * 17)", "(k * 13)"),
        ("(frameIndex * 17)", "(frameIndex * 13)"),
    ):
        assert sample.count(old) == 1
    env = apply_profile_env({}, PROFILE_SETTINGS["max"], rustflags="-Cprofile-use=/tmp/merged.profdata")
    assert env["CARGO_PROFILE_RELEASE_PANIC"] == "unwind"
    assert env["CARGO_PROFILE_RELEASE_OPT_LEVEL"] == "3"
    assert "target-cpu" not in env["RUSTFLAGS"]
    assert "-Cprofile-use=" in env["RUSTFLAGS"]
    native_env = apply_profile_env({}, PROFILE_SETTINGS["speed-native"])
    assert native_env["RUSTFLAGS"] == "-Ctarget-cpu=native"
    assert "W3COS_NO_STRIP" not in env
    fake = spread([1.0, 2.0, 3.0])
    assert format_stat(fake, "steady_rss_kb").startswith("0.0 MB")
    print("self-check ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--jobs", type=int, default=int(os.environ.get("CARGO_BUILD_JOBS", "2")))
    parser.add_argument("--workloads", default=",".join(WORKLOADS))
    parser.add_argument("--profiles", default=",".join(PRIMARY_PROFILES))
    parser.add_argument(
        "--target-cpu-native",
        action="store_true",
        help="Add a speed profile built with -Ctarget-cpu=native. Reported as its own column, not as a shipped profile.",
    )
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--native-binary", type=Path)
    parser.add_argument("--build-root", type=Path, default=Path("/tmp/w3cos-native-browser-build"))
    parser.add_argument("--output", type=Path, default=Path("/tmp/w3cos-native-browser-results.json"))
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return 0
    if args.runs < 5:
        raise SystemExit("at least 5 runs are required")
    workloads = [item.strip() for item in args.workloads.split(",") if item.strip()]
    unknown = [item for item in workloads if item not in WORKLOADS]
    if unknown:
        raise SystemExit(f"unknown workloads: {unknown}")
    profiles = [item.strip() for item in args.profiles.split(",") if item.strip()]
    if args.target_cpu_native and "speed-native" not in profiles:
        profiles.append("speed-native")
    unknown_profiles = [item for item in profiles if item not in PROFILE_SETTINGS]
    if unknown_profiles:
        raise SystemExit(f"unknown profiles: {unknown_profiles}")
    if args.native_binary and len(profiles) != 1:
        raise SystemExit("--native-binary requires exactly one --profiles value")
    repo = args.repo_root.resolve()
    source = (repo / "tools" / "perf-native-browser" / "workloads.ts").read_text()
    timeouts = TIMEOUTS_MS
    binaries = {}
    identities = {}
    if args.native_binary:
        binaries[profiles[0]] = args.native_binary
        identities[profiles[0]] = binary_identity(args.native_binary)
    elif args.skip_build:
        for name in profiles:
            path = args.build_root / f"w3cos-bench-{name}"
            if not path.is_file():
                raise SystemExit(f"missing built binary {path}")
            binaries[name] = path
            identities[name] = binary_identity(path)
    else:
        cli = build_cli(repo, args.jobs)
        for name in profiles:
            binaries[name], identities[name] = build_profile(
                repo, cli, args.jobs, args.build_root, name, workloads, timeouts
            )
    info = host_info(repo)
    info["profiles"] = identities
    from playwright.sync_api import sync_playwright
    server = WorkloadServer(source)
    server.start()
    by_profile = {name: [] for name in profiles}
    try:
        with sync_playwright() as playwright:
            for workload in workloads:
                native_runs = {name: [] for name in profiles}
                browser_runs = []
                for index in range(args.runs):
                    for name in profiles:
                        print(f"== {workload} run {index + 1}/{args.runs} {name}", flush=True)
                        native_runs[name].append(
                            run_native(binaries[name], workload, timeouts[workload] / 1000.0)
                        )
                    print(f"== {workload} run {index + 1}/{args.runs} chromium", flush=True)
                    try:
                        browser_runs.append(run_browser(playwright, server, workload, timeouts[workload]))
                    except Exception as error:
                        browser_runs.append({"status": "error", "error": str(error)})
                for name in profiles:
                    by_profile[name].append(summarize_runs(workload, native_runs[name], browser_runs))
                print(format_profile_comparison(profiles, {name: by_profile[name][-1:] for name in profiles}), flush=True)
    finally:
        server.close()
    payload = {
        "host": info,
        "runs": args.runs,
        "profiles": profiles,
        "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
        "summaries": by_profile,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2) + "\n")
    table = format_profile_comparison(profiles, by_profile)
    print(table)
    for name, identity in identities.items():
        print(f"binary {name}: {identity['bytes']} bytes sha256={identity['sha256']}")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
