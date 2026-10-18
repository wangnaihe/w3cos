//! Leak CI: per-scenario heap diff and long soak (M1 item 7).
//!
//! ```text
//! cargo run --release -p w3cos-compiler --example leak_ci -- \
//!     [--iterations N] [--soak-secs S] [--report-dir DIR] [--scenarios FILE]
//! ```
//!
//! AOT-compiles `leak-scenarios/scenarios.ts` into a core-only crate built
//! with `w3cos-core/leak-detector`, then for every `scenario_<name>` export:
//! two warm-up calls (module/prototype state may legitimately appear), a
//! baseline (`leak_detector::mark_baseline`), N more calls, and the live arena
//! object / array / function counts must equal the baseline exactly. A
//! failing scenario prints the leak detector's report (cycles with full
//! chains, residuals with retainer chains). Scenarios listed in
//! `leak-scenarios/known-leaks.txt` (`name: reason`) are reported but do not
//! fail the run (the plan's registered-cache allowlist).
//!
//! `--soak-secs S` (or `W3COS_LEAK_SOAK_SECS`) then loops all scenarios for
//! S seconds, sampling RSS once a minute, and fails if RSS grows more than 5%
//! over the post-warm-up sample or any live count ends above the baseline.
//! Reports go to `--report-dir` (`leak-summary.json`, `leak-report.txt`).

use std::path::{Path, PathBuf};
use std::process::Command;

use w3cos_compiler::esm_codegen::generate_with_bodies;
use w3cos_compiler::esm_resolver::{EsmBundle, EsmResolver};

const HARNESS: &str = r#####"
mod leak_ci_harness {
    use std::io::Write;
    use w3cos_core::{Value, leak_detector, page_arena};

    type Scenario = (&'static str, fn(Vec<Value>) -> Value);

    fn live() -> (usize, usize, usize) {
        (page_arena::live_objects(), page_arena::live_arrays(), page_arena::live_functions())
    }

    fn call(f: fn(Vec<Value>) -> Value) {
        let _ = f(Vec::new());
        w3cos_core::promise::drain_microtasks();
    }

    fn rss_kb() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }

    fn env_u64(name: &str, default: u64) -> u64 {
        std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
    }

    pub fn run(scenarios: &[Scenario], known: &[(&str, &str)]) -> i32 {
        let iterations = env_u64("W3COS_LEAK_ITERATIONS", 20);
        let soak_secs = env_u64("W3COS_LEAK_SOAK_SECS", 0);
        let dir = std::env::var("W3COS_LEAK_REPORT_DIR").unwrap_or_else(|_| "leak-reports".into());
        let _ = std::fs::create_dir_all(&dir);
        let mut text = String::new();
        let mut results = Vec::new();
        let mut failures = 0;
        for (name, f) in scenarios {
            call(*f);
            call(*f);
            let base = live();
            leak_detector::mark_baseline();
            w3cos_core::ic::reset_stats();
            w3cos_core::typed::reset_stats();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                call(*f);
            }
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let ic = w3cos_core::ic::stats();
            let typed = w3cos_core::typed::stats();
            let after = live();
            let residual = (
                after.0 as i64 - base.0 as i64,
                after.1 as i64 - base.1 as i64,
                after.2 as i64 - base.2 as i64,
            );
            let clean = residual == (0, 0, 0);
            let known_reason = known.iter().find(|(known, _)| known == name).map(|(_, reason)| *reason);
            let status = if clean { "ok" } else if known_reason.is_some() { "known" } else { "LEAK" };
            let line = format!(
                "{status:5} {name:40} baseline objs/arrs/fns {}/{}/{} -> {}/{}/{} (residual {:+}/{:+}/{:+}) over {iterations} iterations, {ms:.1} ms, ic hit {:.1}% ({}/{}), typed calls {} guard failures {} ({:.1}%)",
                base.0, base.1, base.2, after.0, after.1, after.2, residual.0, residual.1, residual.2,
                ic.hit_rate() * 100.0, ic.hits(), ic.hits() + ic.misses(),
                typed.typed, typed.guard_failures, typed.guard_failure_rate() * 100.0
            );
            println!("{line}");
            text.push_str(&line);
            text.push('\n');
            let mut entry = serde_json::json!({
                "ic_hits": ic.hits(),
                "ic_misses": ic.misses(),
                "typed_calls": typed.typed,
                "typed_guard_failures": typed.guard_failures,
                "scenario": name,
                "status": status,
                "iterations": iterations,
                "baseline": { "objects": base.0, "arrays": base.1, "functions": base.2 },
                "after": { "objects": after.0, "arrays": after.1, "functions": after.2 },
                "ms": ms,
            });
            if !clean {
                let report = leak_detector::report();
                let report_text = report.to_text();
                eprintln!("{report_text}");
                text.push_str(&report_text);
                entry["report"] = report.to_json();
                if let Some(reason) = known_reason {
                    entry["known_reason"] = serde_json::Value::from(reason);
                } else {
                    failures += 1;
                }
            }
            leak_detector::clear_baseline();
            results.push(entry);
        }
        let mut soak = serde_json::Value::Null;
        if soak_secs > 0 {
            let base = live();
            // RSS baseline after a warm-up (10% of the soak, capped at 5 min) so
            // allocator/page-cache growth on the first rounds is not counted.
            let warmup = (soak_secs / 10).clamp(1, 300);
            let warm_started = std::time::Instant::now();
            while warm_started.elapsed().as_secs() < warmup {
                for (_, f) in scenarios {
                    call(*f);
                }
            }
            let start_rss = rss_kb().unwrap_or(0);
            println!("soak warm-up {warmup}s done, baseline rss={start_rss} kB");
            let started = std::time::Instant::now();
            let mut last_sample = started;
            let mut samples = vec![serde_json::json!({ "secs": 0, "rss_kb": start_rss })];
            let mut rounds = 0u64;
            while started.elapsed().as_secs() < soak_secs {
                for (_, f) in scenarios {
                    call(*f);
                }
                rounds += 1;
                if last_sample.elapsed().as_secs() >= 60 {
                    last_sample = std::time::Instant::now();
                    let rss = rss_kb().unwrap_or(0);
                    println!("soak t={}s rounds={rounds} rss={rss} kB live={:?}", started.elapsed().as_secs(), live());
                    samples.push(serde_json::json!({ "secs": started.elapsed().as_secs(), "rss_kb": rss, "live": live() }));
                }
            }
            let end_rss = rss_kb().unwrap_or(0);
            let after = live();
            let growth = if start_rss > 0 { (end_rss as f64 - start_rss as f64) / start_rss as f64 * 100.0 } else { 0.0 };
            let rss_ok = growth <= 5.0;
            let live_ok = after.0 <= base.0 && after.1 <= base.1 && after.2 <= base.2;
            let line = format!(
                "soak {}s rounds={rounds} rss {start_rss} -> {end_rss} kB ({growth:+.2}%, limit +5%) live {:?} -> {:?}: {}",
                soak_secs, base, after, if rss_ok && live_ok { "ok" } else { "FAIL" }
            );
            println!("{line}");
            text.push_str(&line);
            text.push('\n');
            if !(rss_ok && live_ok) {
                failures += 1;
                let report = leak_detector::report();
                text.push_str(&report.to_text());
            }
            soak = serde_json::json!({
                "secs": soak_secs, "rounds": rounds, "rss_start_kb": start_rss, "rss_end_kb": end_rss,
                "rss_growth_percent": growth, "live_before": base, "live_after": after,
                "ok": rss_ok && live_ok, "samples": samples,
            });
        }
        let summary = serde_json::json!({ "failures": failures, "scenarios": results, "soak": soak });
        let _ = std::fs::write(format!("{dir}/leak-summary.json"), serde_json::to_string_pretty(&summary).unwrap());
        if let Ok(mut file) = std::fs::File::create(format!("{dir}/leak-report.txt")) {
            let _ = file.write_all(text.as_bytes());
        }
        println!("leak ci: {} scenario(s), {failures} failure(s)", scenarios.len());
        if failures == 0 { 0 } else { 1 }
    }
}
"#####;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("../..").canonicalize().unwrap();
    let scenarios_ts = arg(&args, "--scenarios")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("leak-scenarios/scenarios.ts"));
    let known_file = scenarios_ts.with_file_name("known-leaks.txt");
    let report_dir =
        PathBuf::from(arg(&args, "--report-dir").unwrap_or_else(|| "leak-reports".into()));
    let report_dir = std::env::current_dir().unwrap().join(report_dir);
    let iterations = arg(&args, "--iterations")
        .or_else(|| std::env::var("W3COS_LEAK_ITERATIONS").ok())
        .unwrap_or_else(|| "20".into());
    let soak = arg(&args, "--soak-secs")
        .or_else(|| std::env::var("W3COS_LEAK_SOAK_SECS").ok())
        .unwrap_or_else(|| "0".into());

    let source = std::fs::read_to_string(&scenarios_ts).expect("scenarios file");
    let mut names = Vec::new();
    for line in source.lines() {
        let line = line.trim_start();
        let rest = line
            .strip_prefix("export async function scenario_")
            .or_else(|| line.strip_prefix("export function scenario_"));
        if let Some(rest) = rest {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            names.push(format!("scenario_{name}"));
        }
    }
    assert!(
        !names.is_empty(),
        "no `export function scenario_*` in {}",
        scenarios_ts.display()
    );
    let known = std::fs::read_to_string(&known_file)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            line.split_once(':')
                .map(|(n, r)| (n.trim().to_string(), r.trim().to_string()))
        })
        .collect::<Vec<_>>();

    let root = scenarios_ts.parent().unwrap().to_path_buf();
    let resolver = EsmResolver::new(&root);
    let parsed = resolver
        .parse_graph_from_entry(&scenarios_ts)
        .expect("parse scenarios");
    let bundle = EsmBundle::build(&parsed, &resolver, &scenarios_ts);
    let code = generate_with_bodies(&bundle);

    let crate_dir = workspace.join("target/leak-ci-runner");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    let core = workspace.join("crates/w3cos-core");
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"w3cos-leak-ci-runner\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nw3cos-core = {{ path = {core:?}, features = [\"leak-detector\"] }}\nserde_json = \"1\"\n\n[profile.release]\ndebug = false\n\n[workspace]\n"
        ),
    )
    .unwrap();
    let table = names
        .iter()
        .map(|name| format!("    ({name:?}, m0::m0_{name} as fn(Vec<w3cos_core::Value>) -> w3cos_core::Value),\n"))
        .collect::<String>();
    let known_table = known
        .iter()
        .map(|(name, reason)| format!("    ({name:?}, {reason:?}),\n"))
        .collect::<String>();
    let main_rs = format!(
        "#![allow(dead_code, unused_imports, unused_variables, unused_mut, non_upper_case_globals, unreachable_code, clippy::all)]\n{code}\n{HARNESS}\nfn main() {{\n    let _ = run_entry();\n    w3cos_core::promise::drain_microtasks();\n    let scenarios: &[(&str, fn(Vec<w3cos_core::Value>) -> w3cos_core::Value)] = &[\n{table}    ];\n    let known: &[(&str, &str)] = &[\n{known_table}    ];\n    std::process::exit(leak_ci_harness::run(scenarios, known));\n}}\n"
    );
    std::fs::write(crate_dir.join("src/main.rs"), main_rs).unwrap();
    println!(
        "leak ci: {} scenarios from {}",
        names.len(),
        scenarios_ts.display()
    );
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["run", "--release", "--quiet"])
        .current_dir(&crate_dir)
        .env(
            "CARGO_TARGET_DIR",
            workspace.join("target/leak-ci-runner/target"),
        )
        .env("W3COS_LEAK_ITERATIONS", iterations)
        .env("W3COS_LEAK_SOAK_SECS", soak)
        .env("W3COS_LEAK_REPORT_DIR", &report_dir)
        .status()
        .expect("cargo run leak ci runner");
    std::process::exit(status.code().unwrap_or(1));
}
