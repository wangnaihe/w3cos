mod diff;
mod manifest;
mod runner;
mod server;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use manifest::{SuiteManifest, TestKind};
use runner::{SuiteReport, SuiteRunner, TestReport, build_reftest_report, read_frame, write_frame};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

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
    let cli = Cli::parse();
    let manifest = SuiteManifest::load(&cli.suite)?;
    manifest.validate_checkout(&cli.wpt_root)?;
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
    let mut reports = Vec::with_capacity(manifest.tests.len());
    for (index, test) in manifest.tests.iter().enumerate() {
        let report = match test.kind {
            TestKind::Testharness => run_testharness_worker(cli, index).unwrap_or_else(|error| {
                TestReport::error(
                    test,
                    format!("isolated testharness worker failed: {error:#}"),
                )
            }),
            TestKind::Reftest => {
                run_reftest_workers(cli, manifest, index).unwrap_or_else(|error| {
                    TestReport::error(test, format!("isolated reftest worker failed: {error:#}"))
                })
            }
        };
        reports.push(report);
    }
    Ok(SuiteReport::from_reports(manifest, reports))
}

fn run_testharness_worker(cli: &Cli, index: usize) -> Result<TestReport> {
    let output_path = cli
        .artifacts
        .join(format!(".case-{index}-testharness.json"));
    remove_if_present(&output_path)?;
    let output = spawn_worker(cli, InternalMode::Testharness, index, &output_path, false)?;
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

    let actual_output = spawn_worker(cli, InternalMode::Render, index, &actual_path, false)?;
    require_worker_success(&actual_output).context("actual document render")?;
    let reference_output = spawn_worker(cli, InternalMode::Render, index, &reference_path, true)?;
    require_worker_success(&reference_output).context("reference document render")?;

    let actual = read_frame(&actual_path)?;
    let reference = read_frame(&reference_path)?;
    remove_if_present(&actual_path)?;
    remove_if_present(&reference_path)?;
    build_reftest_report(&manifest.tests[index], &actual, &reference, &cli.artifacts)
}

fn spawn_worker(
    cli: &Cli,
    mode: InternalMode,
    index: usize,
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
    command
        .output()
        .context("failed to launch isolated WPT worker")
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
