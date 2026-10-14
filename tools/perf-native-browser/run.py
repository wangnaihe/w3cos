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
        "native_profile": "opt-level=3 lto=off codegen-units=16 cpu-render",
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


def build_native(repo, cli, jobs, build_root):
    build_root.mkdir(parents=True, exist_ok=True)
    binary = build_root / "w3cos-native-browser-bench"
    env = os.environ.copy()
    env["CARGO_BUILD_JOBS"] = str(jobs)
    env["TMPDIR"] = str(build_root)
    env["CARGO_TARGET_DIR"] = str(repo / "target")
    env["CARGO_PROFILE_RELEASE_LTO"] = "off"
    env["CARGO_PROFILE_RELEASE_CODEGEN_UNITS"] = "16"
    env["CARGO_PROFILE_RELEASE_OPT_LEVEL"] = "3"
    env["W3COS_RUNTIME_FEATURES"] = "cpu-render"
    env["W3COS_RELEASE_PROFILE"] = "speed"
    env["W3COS_NO_STRIP"] = "1"
    env.pop("W3COS_AOT_WINDOW", None)
    fixture = repo / "tools" / "perf-native-browser" / "workloads.ts"
    subprocess.check_call(
        [str(cli), "build", str(fixture), "--release", "--output", str(binary)],
        cwd=repo,
        env=env,
    )
    return binary


def parse_result_line(text):
    marker = "W3COS_PERF_RESULT "
    index = text.find(marker)
    if index < 0:
        return None
    try:
        return json.loads(text[index + len(marker):].strip())
    except json.JSONDecodeError:
        return None


def run_native(binary, workload, timeout):
    env = os.environ.copy()
    env["W3COS_JS_CONSOLE"] = "1"
    env["W3COS_HEADLESS_PUMP"] = "1"
    env["W3COS_PERF_WORKLOAD"] = workload
    env["W3COS_VIEWPORT_W"] = str(VIEWPORT["width"])
    env["W3COS_VIEWPORT_H"] = str(VIEWPORT["height"])
    env.pop("W3COS_AOT_WINDOW", None)
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
    return {"agreement": agreement, "rows": rows, "native_runs": native_runs, "browser_runs": browser_runs}


def format_stat(stat):
    if not stat or stat.get("median") is None:
        return "n/a"
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
                f"| {row['workload']} | {row['metric']} | {format_stat(row['w3cos'])} | {format_stat(row['chromium'])} | {ratio_text} | {row['comparable']} |"
            )
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
    print("self-check ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--jobs", type=int, default=int(os.environ.get("CARGO_BUILD_JOBS", "2")))
    parser.add_argument("--workloads", default=",".join(WORKLOADS))
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
    repo = args.repo_root.resolve()
    source = (repo / "tools" / "perf-native-browser" / "workloads.ts").read_text()
    if args.native_binary:
        binary = args.native_binary
    elif args.skip_build:
        binary = args.build_root / "w3cos-native-browser-bench"
    else:
        cli = build_cli(repo, args.jobs)
        binary = build_native(repo, cli, args.jobs, args.build_root)
    info = host_info(repo)
    from playwright.sync_api import sync_playwright
    server = WorkloadServer(source)
    server.start()
    timeouts = TIMEOUTS_MS
    summaries = []
    try:
        with sync_playwright() as playwright:
            for workload in workloads:
                native_runs = []
                browser_runs = []
                for index in range(args.runs):
                    print(f"== {workload} run {index + 1}/{args.runs} native", flush=True)
                    native_runs.append(run_native(binary, workload, timeouts[workload] / 1000.0))
                    print(f"== {workload} run {index + 1}/{args.runs} chromium", flush=True)
                    try:
                        browser_runs.append(run_browser(playwright, server, workload, timeouts[workload]))
                    except Exception as error:
                        browser_runs.append({"status": "error", "error": str(error)})
                summaries.append(summarize_runs(workload, native_runs, browser_runs))
                print(format_table([summaries[-1]]), flush=True)
    finally:
        server.close()
    payload = {"host": info, "runs": args.runs, "source_sha256": __import__("hashlib").sha256(source.encode()).hexdigest(), "summaries": summaries}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2) + "\n")
    table = format_table(summaries)
    print(table)
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
