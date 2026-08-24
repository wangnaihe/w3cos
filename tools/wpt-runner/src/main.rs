mod diff;
mod discover;
mod manifest;
mod runner;
mod server;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use discover::discover_suite;
use manifest::{SuiteManifest, TestKind};
use runner::{
    SuiteReport, SuiteRunner, TestReport, build_reftest_report, effective_case_timeout,
    read_frame, write_frame,
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum InternalMode {
    Testharness,
    Render,
}

#[derive(Debug, Parser)]
#[command(
    name = "w3cos-wpt",
    about = "Run a pinned raw Web Platform Tests subset through W3COS"
)]
struct Cli {
    /// Clean web-platform-tests/wpt checkout at the suite's pinned revision.
    #[arg(long)]
    wpt_root: PathBuf,
    /// Versioned W3COS suite manifest.
    #[arg(long, default_value = "tests/wpt/w3cos-smoke.json")]
    suite: PathBuf,
    /// Directory for structured results and reftest PNGs.
    #[arg(long, default_value = "target/wpt-results")]
    artifacts: PathBuf,
    /// Per-navigation and testharness completion timeout.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    /// Produce evidence without making current conformance failures fatal.
    #[arg(long)]
    report_only: bool,
    /// Discover runnable cases below these WPT-relative roots (repeatable).
    #[arg(long = "discover-root")]
    discover_roots: Vec<PathBuf>,
    /// Write a generated suite manifest and exit instead of running cases.
    #[arg(long)]
    discover_output: Option<PathBuf>,
    /// Structured inventory paired with --discover-output.
    #[arg(long)]
    discovery_report: Option<PathBuf>,
    /// Number of isolated case workers to run concurrently.
    #[arg(long, default_value_t = 1)]
    jobs: usize,
    /// Keep PNG evidence only for failing reftests (recommended for large suites).
    #[arg(long)]
    failure_artifacts_only: bool,
    /// Zero-based first case to execute from the suite.
    #[arg(long, default_value_t = 0)]
    case_start: usize,
    /// Maximum cases to execute; omitted means through the end of the suite.
    #[arg(long)]
    case_limit: Option<usize>,
    /// Merge structured reports from completed case ranges (repeatable).
    #[arg(long = "merge-report")]
    merge_reports: Vec<PathBuf>,
    /// Output path for --merge-report.
    #[arg(long)]
    merge_output: Option<PathBuf>,
    /// Replace matching paths after merging (repeatable rerun reports).
    #[arg(long = "merge-replacement")]
    merge_replacements: Vec<PathBuf>,
    #[arg(long, hide = true)]
    internal_mode: Option<InternalMode>,
    #[arg(long, hide = true)]
    internal_case_index: Option<usize>,
    #[arg(long, hide = true)]
    internal_output: Option<PathBuf>,
    #[arg(long, hide = true)]
    internal_reference: bool,
}

fn main() -> Result<()> {
    install_js_exception_panic_filter();
    let cli = Cli::parse();
    if !cfg!(panic = "unwind") {
        bail!(
            "w3cos-wpt requires panic=unwind because JavaScript exceptions use Rust unwinding; run with `cargo run --profile wpt -p w3cos-wpt-runner -- ...`"
        );
    }
    if cli.jobs == 0 {
        bail!("--jobs must be at least 1");
    }
    let manifest = SuiteManifest::load(&cli.suite)?;
    manifest.validate_checkout(&cli.wpt_root)?;
    if !cli.merge_reports.is_empty() {
        return merge_reports(&cli, &manifest);
    }
    if let Some(output) = &cli.discover_output {
        let roots = if cli.discover_roots.is_empty() {
            vec![PathBuf::from("dom/nodes"), PathBuf::from("css/CSS2")]
        } else {
            cli.discover_roots.clone()
        };
        let (suite, discovery) = discover_suite(&cli.wpt_root, &manifest, &roots)?;
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(output, serde_json::to_vec_pretty(&suite)?)
            .with_context(|| format!("failed to write discovered suite {}", output.display()))?;
        let report = cli
            .discovery_report
            .clone()
            .unwrap_or_else(|| output.with_extension("inventory.json"));
        if let Some(parent) = report.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&report, serde_json::to_vec_pretty(&discovery)?)
            .with_context(|| format!("failed to write discovery report {}", report.display()))?;
        println!(
            "W3COS_WPT_DISCOVERY runnable={} limited={} other={} suite={} report={}",
            discovery.runnable,
            discovery.limited,
            discovery.other,
            output.display(),
            report.display()
        );
        return Ok(());
    }
    if let Some(mode) = cli.internal_mode {
        return run_worker(&cli, manifest, mode);
    }

    let report = run_isolated_suite(&cli, &manifest)?;
    let report_path = cli.artifacts.join("results.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("failed to write WPT report {}", report_path.display()))?;
    println!(
        "W3COS_WPT_RESULT passed={} failed={} report={}",
        report.passed,
        report.failed,
        report_path.display()
    );
    if !report.is_pass() && !cli.report_only {
        bail!("WPT suite has {} failing cases", report.failed);
    }
    Ok(())
}

fn install_js_exception_panic_filter() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info
            .payload()
            .downcast_ref::<w3cos_core::PanicValue>()
            .is_none()
        {
            previous(info);
        }
    }));
}

fn merge_reports(cli: &Cli, manifest: &SuiteManifest) -> Result<()> {
    let output = cli
        .merge_output
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--merge-report requires --merge-output"))?;
    let mut tests = Vec::new();
    for path in &cli.merge_reports {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read WPT batch report {}", path.display()))?;
        let report: SuiteReport = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid WPT batch report {}", path.display()))?;
        if !report_matches_manifest(&report, manifest) {
            bail!(
                "WPT batch report does not match the selected suite: {}",
                path.display()
            );
        }
        tests.extend(report.tests);
    }
    if tests.len() != manifest.tests.len() {
        bail!(
            "merged WPT report count mismatch: expected {}, observed {}",
            manifest.tests.len(),
            tests.len()
        );
    }
    for (expected, observed) in manifest.tests.iter().zip(&tests) {
        if expected.path != observed.path || expected.kind != observed.kind {
            bail!(
                "merged WPT report order mismatch: expected {}, observed {}",
                expected.path,
                observed.path
            );
        }
    }
    for path in &cli.merge_replacements {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read WPT replacement report {}", path.display()))?;
        let replacement: SuiteReport = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid WPT replacement report {}", path.display()))?;
        if !report_matches_manifest(&replacement, manifest) {
            bail!(
                "WPT replacement report does not match the selected suite: {}",
                path.display()
            );
        }
        for replacement_test in replacement.tests {
            let Some(index) = manifest.tests.iter().position(|test| {
                test.path == replacement_test.path && test.kind == replacement_test.kind
            }) else {
                bail!(
                    "WPT replacement path is not in the suite: {}",
                    replacement_test.path
                );
            };
            tests[index] = replacement_test;
        }
    }
    let report = SuiteReport::from_reports(manifest, tests);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("failed to write merged WPT report {}", output.display()))?;
    println!(
        "W3COS_WPT_MERGED passed={} failed={} total={} report={}",
        report.passed,
        report.failed,
        report.tests.len(),
        output.display()
    );
    Ok(())
}

fn report_matches_manifest(report: &SuiteReport, manifest: &SuiteManifest) -> bool {
    report.schema_version == 1
        && report.upstream == manifest.upstream
        && report.revision == manifest.revision
        && report.viewport_width == manifest.viewport.width
        && report.viewport_height == manifest.viewport.height
}

fn run_worker(cli: &Cli, manifest: SuiteManifest, mode: InternalMode) -> Result<()> {
    let index = cli
        .internal_case_index
        .ok_or_else(|| anyhow::anyhow!("internal worker requires a case index"))?;
    let output = cli
        .internal_output
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("internal worker requires an output path"))?;
    let runner = SuiteRunner::new(
        cli.wpt_root.clone(),
        manifest,
        Duration::from_millis(cli.timeout_ms),
    );
    match mode {
        InternalMode::Testharness => {
            let report = runner.run_testharness_case(index)?;
            std::fs::write(output, serde_json::to_vec(&report)?)
                .with_context(|| format!("failed to write WPT worker report {}", output.display()))
        }
        InternalMode::Render => {
            let frame = runner.render_case_document(index, cli.internal_reference)?;
            write_frame(output, &frame)
        }
    }
}

fn run_isolated_suite(cli: &Cli, manifest: &SuiteManifest) -> Result<SuiteReport> {
    std::fs::create_dir_all(&cli.artifacts).with_context(|| {
        format!(
            "failed to create WPT artifact directory {}",
            cli.artifacts.display()
        )
    })?;
    let start = cli.case_start.min(manifest.tests.len());
    let end = cli
        .case_limit
        .map(|limit| start.saturating_add(limit).min(manifest.tests.len()))
        .unwrap_or(manifest.tests.len());
    if start == end {
        bail!("selected WPT case range is empty");
    }
    let selected = end - start;
    let next = AtomicUsize::new(start);
    let completed = AtomicUsize::new(0);
    let reports = Mutex::new(
        (0..selected)
            .map(|_| None)
            .collect::<Vec<Option<TestReport>>>(),
    );
    let worker_count = cli.jobs.min(selected).max(1);
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= end {
                        break;
                    }
                    let test = &manifest.tests[index];
                    let report = run_isolated_case(cli, manifest, index, test);
                    reports.lock().expect("WPT report lock")[index - start] = Some(report);
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if done.is_multiple_of(100) || done == selected {
                        eprintln!(
                            "W3COS_WPT_PROGRESS completed={} total={} suite_start={}",
                            done, selected, start
                        );
                    }
                }
            });
        }
    });
    let reports = reports
        .into_inner()
        .expect("WPT report lock")
        .into_iter()
        .map(|report| report.expect("isolated WPT worker did not record a result"))
        .collect();
    Ok(SuiteReport::from_reports(manifest, reports))
}

fn run_isolated_case(
    cli: &Cli,
    manifest: &SuiteManifest,
    index: usize,
    test: &manifest::TestCase,
) -> TestReport {
    match test.kind {
        TestKind::Testharness => run_testharness_worker(cli, index, test).unwrap_or_else(|error| {
            TestReport::error(
                test,
                format!("isolated testharness worker failed: {error:#}"),
            )
        }),
        TestKind::Reftest => run_reftest_workers(cli, manifest, index).unwrap_or_else(|error| {
            TestReport::error(test, format!("isolated reftest worker failed: {error:#}"))
        }),
    }
}

fn run_testharness_worker(
    cli: &Cli,
    index: usize,
    test: &manifest::TestCase,
) -> Result<TestReport> {
    let output_path = cli
        .artifacts
        .join(format!(".case-{index}-testharness.json"));
    remove_if_present(&output_path)?;
    let output = spawn_worker(
        cli,
        InternalMode::Testharness,
        index,
        test,
        &output_path,
        false,
    )?;
    require_worker_success(&output)?;
    let bytes = std::fs::read(&output_path)
        .with_context(|| format!("testharness worker did not write {}", output_path.display()))?;
    remove_if_present(&output_path)?;
    serde_json::from_slice(&bytes).context("invalid isolated testharness report")
}

fn run_reftest_workers(cli: &Cli, manifest: &SuiteManifest, index: usize) -> Result<TestReport> {
    let actual_path = cli.artifacts.join(format!(".case-{index}-actual.bin"));
    let reference_path = cli.artifacts.join(format!(".case-{index}-reference.bin"));
    remove_if_present(&actual_path)?;
    remove_if_present(&reference_path)?;

    let actual_output = spawn_worker(
        cli,
        InternalMode::Render,
        index,
        &manifest.tests[index],
        &actual_path,
        false,
    )?;
    require_worker_success(&actual_output).context("actual document render")?;
    let reference_output = spawn_worker(
        cli,
        InternalMode::Render,
        index,
        &manifest.tests[index],
        &reference_path,
        true,
    )?;
    require_worker_success(&reference_output).context("reference document render")?;

    let actual = read_frame(&actual_path)?;
    let reference = read_frame(&reference_path)?;
    remove_if_present(&actual_path)?;
    remove_if_present(&reference_path)?;
    build_reftest_report(
        &manifest.tests[index],
        &actual,
        &reference,
        &cli.artifacts,
        cli.failure_artifacts_only,
    )
}

fn spawn_worker(
    cli: &Cli,
    mode: InternalMode,
    index: usize,
    test: &manifest::TestCase,
    output: &Path,
    reference: bool,
) -> Result<Output> {
    let executable = std::env::current_exe().context("failed to locate WPT runner executable")?;
    let mut command = Command::new(executable);
    command
        .arg("--wpt-root")
        .arg(&cli.wpt_root)
        .arg("--suite")
        .arg(&cli.suite)
        .arg("--timeout-ms")
        .arg(cli.timeout_ms.to_string())
        .arg("--internal-mode")
        .arg(match mode {
            InternalMode::Testharness => "testharness",
            InternalMode::Render => "render",
        })
        .arg("--internal-case-index")
        .arg(index.to_string())
        .arg("--internal-output")
        .arg(output);
    if reference {
        command.arg("--internal-reference");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch isolated WPT worker")?;
    let mut stdout = child.stdout.take().context("worker stdout pipe")?;
    let mut stderr = child.stderr.take().context("worker stderr pipe")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let case_timeout = effective_case_timeout(
        &cli.wpt_root,
        test,
        Duration::from_millis(cli.timeout_ms),
    );
    let worker_budget = match mode {
        // A harness worker first navigates and then waits for test completion.
        InternalMode::Testharness => case_timeout.saturating_mul(2),
        InternalMode::Render => case_timeout,
    } + Duration::from_secs(2);
    let deadline = Instant::now() + worker_budget;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().context("failed to poll isolated WPT worker")? {
            break (status, false);
        }
        if Instant::now() >= deadline {
            child.kill().context("failed to terminate timed-out WPT worker")?;
            break (
                child.wait().context("failed to reap timed-out WPT worker")?,
                true,
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("WPT worker stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("WPT worker stderr reader panicked"))??;
    if timed_out {
        let tail = String::from_utf8_lossy(&stderr)
            .chars()
            .rev()
            .take(4_000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        bail!(
            "worker exceeded its {} ms outer deadline: {}",
            worker_budget.as_millis(),
            tail.trim()
        );
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn require_worker_success(output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail = stderr
        .chars()
        .rev()
        .take(4_000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    bail!("worker exited with {}: {}", output.status, tail.trim())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove temporary WPT artifact {}", path.display())),
    }
}
