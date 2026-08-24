use crate::diff::{PixelDiff, compare_frames};
use crate::manifest::{ReferenceRelation, SuiteManifest, TestCase, TestKind};
use crate::server::StaticServer;
use anyhow::{Context, Result, anyhow, bail};
use image::{ColorType, ImageFormat};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use w3cos_runtime::dynamic_script::{
    DocumentLoadProgress, DocumentLoader, DocumentLoaderOptions, ScriptPolicy,
};
use w3cos_runtime::headless::HeadlessFrame;

const RESULT_GLOBAL: &str = "__w3cos_wpt_results_json";
const PARTIAL_RESULT_GLOBAL: &str = "__w3cos_wpt_partial_results_json";
const HEADLESS_FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Debug, Deserialize, Serialize)]
pub struct SuiteReport {
    pub schema_version: u32,
    pub upstream: String,
    pub revision: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub passed: usize,
    pub failed: usize,
    pub tests: Vec<TestReport>,
}

impl SuiteReport {
    pub fn from_reports(manifest: &SuiteManifest, tests: Vec<TestReport>) -> Self {
        let passed = tests
            .iter()
            .filter(|report| report.status == CaseStatus::Pass)
            .count();
        let failed = tests.len() - passed;
        Self {
            schema_version: 1,
            upstream: manifest.upstream.clone(),
            revision: manifest.revision.clone(),
            viewport_width: manifest.viewport.width,
            viewport_height: manifest.viewport.height,
            passed,
            failed,
            tests,
        }
    }

    pub fn is_pass(&self) -> bool {
        self.failed == 0 && self.passed == self.tests.len()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TestReport {
    pub path: String,
    pub kind: TestKind,
    pub status: CaseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtests: Vec<SubtestReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<ReferenceRelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_diff: Option<PixelDiffReport>,
}

impl TestReport {
    pub fn error(test: &TestCase, message: impl Into<String>) -> Self {
        Self {
            path: test.path.clone(),
            kind: test.kind,
            status: CaseStatus::Error,
            message: Some(message.into()),
            subtests: Vec::new(),
            reference: test.reference.clone(),
            relation: test.relation,
            pixel_diff: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Pass,
    Fail,
    Error,
    Timeout,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SubtestReport {
    pub name: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PixelDiffReport {
    pub max_difference: u8,
    pub different_pixels: u64,
    pub allowed_max_difference: u8,
    pub allowed_different_pixels: u64,
}

#[derive(Debug, Deserialize)]
struct HarnessPayload {
    #[serde(default)]
    harness_status: Option<u8>,
    #[serde(default)]
    harness_message: Option<String>,
    tests: Vec<HarnessSubtest>,
}

#[derive(Debug, Deserialize)]
struct HarnessSubtest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: Option<u8>,
    #[serde(default)]
    message: Option<String>,
}

pub struct SuiteRunner {
    root: PathBuf,
    manifest: SuiteManifest,
    timeout: Duration,
}

impl SuiteRunner {
    pub fn new(root: PathBuf, manifest: SuiteManifest, timeout: Duration) -> Self {
        Self {
            root,
            manifest,
            timeout,
        }
    }

    pub fn run_testharness_case(&self, index: usize) -> Result<TestReport> {
        let test = self
            .manifest
            .tests
            .get(index)
            .ok_or_else(|| anyhow!("WPT case index {index} is out of range"))?;
        if test.kind != TestKind::Testharness {
            bail!("WPT case {} is not a testharness test", test.path);
        }
        let server = StaticServer::start(self.root.clone())?;
        self.run_testharness(&server, test)
    }

    pub fn render_case_document(&self, index: usize, reference: bool) -> Result<HeadlessFrame> {
        let test = self
            .manifest
            .tests
            .get(index)
            .ok_or_else(|| anyhow!("WPT case index {index} is out of range"))?;
        if test.kind != TestKind::Reftest {
            bail!("WPT case {} is not a reftest", test.path);
        }
        let path = if reference {
            test.reference
                .as_deref()
                .ok_or_else(|| anyhow!("reftest {} has no reference", test.path))?
        } else {
            &test.path
        };
        let server = StaticServer::start(self.root.clone())?;
        load_and_render(
            &server.url_for(path),
            effective_document_timeout(&self.root, path, self.timeout),
            self.manifest.viewport.width,
            self.manifest.viewport.height,
        )
    }

    fn run_testharness(&self, server: &StaticServer, test: &TestCase) -> Result<TestReport> {
        let timeout = effective_case_timeout(&self.root, test, self.timeout);
        let _document = load_document(
            &server.url_for(&test.path),
            timeout,
            self.manifest.viewport.width,
            self.manifest.viewport.height,
        )?;
        let payload = wait_for_harness_payload(timeout)?;
        let minimum = test.expected_subtests_min.unwrap_or(1);
        let subtests = payload
            .tests
            .iter()
            .map(|subtest| SubtestReport {
                name: subtest.name.clone(),
                status: harness_status_name(subtest.status).to_string(),
                message: subtest.message.clone().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let passed = payload.harness_status == Some(0)
            && payload.tests.len() >= minimum
            && payload.tests.iter().all(|test| test.status == Some(0));
        let message = if passed {
            None
        } else if payload.tests.len() < minimum {
            Some(format!(
                "expected at least {minimum} subtests, observed {}",
                payload.tests.len()
            ))
        } else if payload
            .harness_message
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            Some(format!(
                "harness={}, failing_subtests={}",
                harness_status_name(payload.harness_status),
                payload
                    .tests
                    .iter()
                    .filter(|test| test.status != Some(0))
                    .count()
            ))
        } else {
            payload.harness_message
        };
        Ok(TestReport {
            path: test.path.clone(),
            kind: test.kind,
            status: if passed {
                CaseStatus::Pass
            } else if payload.harness_status == Some(2) {
                CaseStatus::Timeout
            } else {
                CaseStatus::Fail
            },
            message,
            subtests,
            reference: None,
            relation: None,
            pixel_diff: None,
        })
    }
}

/// WPT's `<meta name=timeout content=long>` grants six times the normal
/// harness budget. Honor it in both the in-process loader and the isolated
/// worker deadline so assertion-heavy conformance pages do not become false
/// infrastructure errors.
pub fn effective_case_timeout(root: &Path, test: &TestCase, default: Duration) -> Duration {
    let mut timeout = effective_document_timeout(root, &test.path, default);
    if let Some(reference) = test.reference.as_deref() {
        timeout = timeout.max(effective_document_timeout(root, reference, default));
    }
    timeout
}

fn effective_document_timeout(root: &Path, relative: &str, default: Duration) -> Duration {
    let Ok(source) = std::fs::read_to_string(root.join(relative)) else {
        return default;
    };
    if declares_long_timeout(&source) {
        default.saturating_mul(6)
    } else {
        default
    }
}

fn declares_long_timeout(source: &str) -> bool {
    source.split('>').any(|tag| {
        let normalized = tag
            .chars()
            .filter(|character| {
                !character.is_ascii_whitespace() && !matches!(character, '\'' | '"')
            })
            .flat_map(char::to_lowercase)
            .collect::<String>();
        normalized.contains("<meta")
            && normalized.contains("name=timeout")
            && normalized.contains("content=long")
    })
}

pub fn build_reftest_report(
    test: &TestCase,
    actual: &HeadlessFrame,
    expected: &HeadlessFrame,
    artifacts: &Path,
    failure_artifacts_only: bool,
) -> Result<TestReport> {
    let reference = test
        .reference
        .as_deref()
        .ok_or_else(|| anyhow!("reftest {} has no reference", test.path))?;
    let relation = test
        .relation
        .ok_or_else(|| anyhow!("reftest {} has no relation", test.path))?;
    let (diff, diff_rgba) = compare_frames(actual, expected, test.fuzzy)?;
    let passed = match relation {
        ReferenceRelation::Match => diff.within_fuzzy,
        ReferenceRelation::Mismatch => !diff.within_fuzzy,
    };
    if !failure_artifacts_only || !passed {
        write_reftest_artifacts(test, actual, expected, &diff_rgba, artifacts)?;
    }
    Ok(TestReport {
        path: test.path.clone(),
        kind: test.kind,
        status: if passed {
            CaseStatus::Pass
        } else {
            CaseStatus::Fail
        },
        message: (!passed).then(|| {
            format!(
                "reference {:?} condition failed: max_difference={}, different_pixels={}",
                relation, diff.max_difference, diff.different_pixels
            )
        }),
        subtests: Vec::new(),
        reference: Some(reference.to_string()),
        relation: Some(relation),
        pixel_diff: Some(pixel_diff_report(diff, test)),
    })
}

fn write_reftest_artifacts(
    test: &TestCase,
    actual: &HeadlessFrame,
    expected: &HeadlessFrame,
    diff: &[u8],
    artifacts: &Path,
) -> Result<()> {
    let stem = artifact_stem(&test.path);
    write_png(&artifacts.join(format!("{stem}-actual.png")), actual)?;
    write_png(&artifacts.join(format!("{stem}-expected.png")), expected)?;
    image::save_buffer_with_format(
        artifacts.join(format!("{stem}-diff.png")),
        diff,
        actual.width,
        actual.height,
        ColorType::Rgba8,
        ImageFormat::Png,
    )
    .context("failed to write WPT diff PNG")?;
    Ok(())
}

fn load_and_render(url: &str, timeout: Duration, width: u32, height: u32) -> Result<HeadlessFrame> {
    // Keep the navigation loader alive until the frame has been captured. Its
    // owned stylesheet/font/resource state is intentionally released on Drop.
    let _document = load_document(url, timeout, width, height)?;
    w3cos_runtime::headless::render_document_rgba(width, height)
}

fn load_document(url: &str, timeout: Duration, width: u32, height: u32) -> Result<DocumentLoader> {
    let mut script_policy = ScriptPolicy::default();
    // The runner's explicit per-case timeout is the authoritative budget.
    // Keeping the production VM's five-second / one-million-instruction
    // defaults here makes assertion-heavy WPT files fail before the harness
    // or isolated outer worker deadline.
    script_policy.limits.max_wall_time = Some(timeout);
    // Some upstream `timeout=long` tests deliberately execute hundreds of
    // millions of simple operations to exercise optimized collection paths.
    // The isolated worker and VM wall-clock deadlines already provide the
    // authoritative safety bound, so the instruction ceiling must stay above
    // those valid workloads instead of turning them into false conformance
    // failures.
    script_policy.limits.max_instructions = u64::MAX;
    let max_heap_mib = std::env::var("W3COS_WPT_MAX_HEAP_MIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256);
    script_policy.limits.max_heap_bytes = Some(max_heap_mib * 1024 * 1024);
    let mut loader = DocumentLoader::new(
        script_policy,
        DocumentLoaderOptions {
            timeout_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
            ..DocumentLoaderOptions::default()
        },
    );
    loader.navigate(url)?;
    w3cos_runtime::jsdom::set_viewport(f64::from(width), f64::from(height));
    let deadline = Instant::now() + timeout;
    loop {
        match loader.poll() {
            DocumentLoadProgress::Complete => return Ok(loader),
            DocumentLoadProgress::Failed(error) => bail!("document load failed for {url}: {error}"),
            DocumentLoadProgress::Cancelled => bail!("document load was cancelled for {url}"),
            _ if Instant::now() >= deadline => bail!("document load timed out for {url}"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
}

fn wait_for_harness_payload(timeout: Duration) -> Result<HarnessPayload> {
    let deadline = Instant::now() + timeout;
    let mut next_animation_frame = Instant::now();
    loop {
        w3cos_runtime::jsdom::drain_microtasks();
        w3cos_runtime::jsdom::tick_timers();
        let now = Instant::now();
        if now >= next_animation_frame {
            w3cos_runtime::jsdom::run_animation_frame();
            w3cos_runtime::jsdom::drain_microtasks();
            next_animation_frame = now + HEADLESS_FRAME_INTERVAL;
        }
        let value = w3cos_runtime::jsdom::window_value().get_property(RESULT_GLOBAL);
        if !value.is_undefined() {
            let json = value.to_js_string();
            if !json.is_empty() {
                return serde_json::from_str(&json)
                    .with_context(|| format!("invalid WPT harness payload: {json}"));
            }
        }
        if Instant::now() >= deadline {
            let partial = w3cos_runtime::jsdom::window_value()
                .get_property(PARTIAL_RESULT_GLOBAL)
                .to_js_string();
            bail!(
                "WPT testharness did not publish a completion payload; partial_results={partial}"
            );
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn harness_status_name(status: Option<u8>) -> &'static str {
    match status {
        Some(0) => "pass",
        Some(1) => "fail",
        Some(2) => "timeout",
        Some(3) => "not_run",
        Some(4) => "precondition_failed",
        _ => "unknown",
    }
}

fn pixel_diff_report(diff: PixelDiff, test: &TestCase) -> PixelDiffReport {
    PixelDiffReport {
        max_difference: diff.max_difference,
        different_pixels: diff.different_pixels,
        allowed_max_difference: test.fuzzy.max_difference,
        allowed_different_pixels: test.fuzzy.total_pixels,
    }
}

fn artifact_stem(path: &str) -> String {
    let mut sanitized = path
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => character,
            _ => '_',
        })
        .collect::<String>();
    sanitized.truncate(160);
    format!("{sanitized}-{:016x}", stable_path_hash(path))
}

fn stable_path_hash(path: &str) -> u64 {
    path.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn write_png(path: &Path, frame: &HeadlessFrame) -> Result<()> {
    image::save_buffer_with_format(
        path,
        &frame.rgba,
        frame.width,
        frame.height,
        ColorType::Rgba8,
        ImageFormat::Png,
    )
    .with_context(|| format!("failed to write WPT PNG {}", path.display()))
}

pub fn write_frame(path: &Path, frame: &HeadlessFrame) -> Result<()> {
    let mut bytes = Vec::with_capacity(frame.rgba.len() + 8);
    bytes.extend_from_slice(&frame.width.to_le_bytes());
    bytes.extend_from_slice(&frame.height.to_le_bytes());
    bytes.extend_from_slice(&frame.rgba);
    std::fs::write(path, bytes)
        .with_context(|| format!("failed to write WPT frame {}", path.display()))
}

pub fn read_frame(path: &Path) -> Result<HeadlessFrame> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read WPT frame {}", path.display()))?;
    if bytes.len() < 8 {
        bail!("WPT frame header is truncated: {}", path.display());
    }
    let width = u32::from_le_bytes(bytes[0..4].try_into().expect("four-byte width"));
    let height = u32::from_le_bytes(bytes[4..8].try_into().expect("four-byte height"));
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow!("WPT frame dimensions overflow: {width}x{height}"))?;
    if bytes.len() != expected + 8 {
        bail!(
            "WPT frame byte length mismatch: expected {}, observed {}",
            expected,
            bytes.len().saturating_sub(8)
        );
    }
    Ok(HeadlessFrame {
        width,
        height,
        rgba: bytes[8..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_fail_closed_for_unknown_values() {
        assert_eq!(harness_status_name(Some(0)), "pass");
        assert_eq!(harness_status_name(Some(99)), "unknown");
        assert_eq!(harness_status_name(None), "unknown");
    }

    #[test]
    fn artifact_names_cannot_create_subdirectories() {
        let first = artifact_stem("css/box/test.html");
        let second = artifact_stem("css_box/test.html");
        assert!(first.starts_with("css_box_test.html-"));
        assert!(!first.contains('/'));
        assert_ne!(first, second);

        let long = artifact_stem(&format!("{}.html", "nested/".repeat(100)));
        assert!(long.len() <= 177);
    }

    #[test]
    fn frame_files_round_trip_and_reject_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frame.bin");
        let frame = HeadlessFrame {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        };
        write_frame(&path, &frame).unwrap();
        assert_eq!(read_frame(&path).unwrap(), frame);

        std::fs::write(&path, [0_u8; 7]).unwrap();
        assert!(read_frame(&path).is_err());
    }

    #[test]
    fn long_timeout_metadata_is_attribute_order_and_quote_independent() {
        for source in [
            "<meta name=timeout content=long>",
            "<META content='long' name=\"timeout\">",
        ] {
            assert!(declares_long_timeout(source));
        }
        assert!(!declares_long_timeout("<meta name=timeout content=normal>"));
    }
}
